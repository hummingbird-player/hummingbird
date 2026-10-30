use std::path::Path;

use cntp_i18n::{I18N_MANAGER, Locale};
use icu_collator::{
    CollatorBorrowed,
    options::{CollatorOptions, Strength},
};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tracing::debug;

const SORT_COLLATION: &str = "HB_SORT";

fn sort_collator(locale: &Locale) -> CollatorBorrowed<'static> {
    let mut options = CollatorOptions::default();
    options.strength = Some(Strength::Secondary);

    let preferences = locale.icu_collate().clone();
    CollatorBorrowed::try_new(preferences.into(), options)
        .or_else(|error| {
            tracing::warn!(
                ?error,
                "no collation data for this locale, using root order"
            );
            CollatorBorrowed::try_new(Default::default(), options)
        })
        .expect("the root collator is always available")
}

pub async fn create_pool(path: impl AsRef<Path>) -> sqlx::Result<SqlitePool> {
    debug!("Creating database pool at {:?}", path.as_ref());
    let locale = I18N_MANAGER.read().unwrap().locale.clone();

    let collation_locale = locale
        .collate
        .first()
        .cloned()
        .unwrap_or_else(|| "en".into());

    let collator = sort_collator(&locale);

    let options = SqliteConnectOptions::new()
        .filename(path)
        .synchronous(SqliteSynchronous::Normal)
        .journal_mode(SqliteJournalMode::Wal)
        .collation(SORT_COLLATION, move |left, right| {
            collator.compare(left, right)
        })
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await?;

    let migrations = sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(&pool)
        .await;

    if let Err(e) = migrations {
        // old Windows databases can hit line-ending caused hash mismatches on these versions,
        // rewrite their checksums and retry, any other migration failure is fatal
        let recoverable = match &e {
            sqlx::migrate::MigrateError::VersionMismatch(v) => {
                cfg!(target_os = "windows")
                    && matches!(
                        v,
                        20240730163128
                            | 20240730163151
                            | 20240730163200
                            | 20240817201809
                            | 20240817201912
                            | 20240917084650
                            | 20250424090924
                            | 20250512214434
                            | 20250512231103
                            | 20250825224757
                            | 20250825225240
                            | 20250825234341
                            | 20251022214837
                    )
            }
            _ => false,
        };
        if !recoverable {
            return Err(e.into());
        }

        let fix_query = include_str!("../../../queries/windows_fix_checksums.sql");
        sqlx::query(fix_query).execute(&pool).await?;

        sqlx::migrate!("./migrations")
            .set_ignore_missing(true)
            .run(&pool)
            .await?;
    }

    ensure_sort_indexes_for_locale(&pool, &collation_locale).await?;

    Ok(pool)
}

/// Rebuilds the sort collation indexes for the given locale if they are not already built for that
/// locale.
async fn ensure_sort_indexes_for_locale(pool: &SqlitePool, locale: &str) -> sqlx::Result<()> {
    let built_locale: Option<String> =
        sqlx::query_scalar("SELECT locale FROM sort_collation_state WHERE id = 1")
            .fetch_optional(pool)
            .await?;

    if let Some(built_locale) = built_locale {
        if built_locale == locale {
            return Ok(());
        }

        debug!("Rebuilding library indexes for collation locale '{locale}'");
        sqlx::query("REINDEX").execute(pool).await?;
    }

    sqlx::query(
        "INSERT INTO sort_collation_state (id, locale) VALUES (1, $1)
         ON CONFLICT(id) DO UPDATE SET locale = excluded.locale",
    )
    .bind(locale)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use cntp_i18n::Locale;

    use super::sort_collator;

    fn sorted(locale: &str, names: &[&str]) -> Vec<String> {
        let collator = sort_collator(&Locale::new_from_locale_identifier(locale));
        let mut names = names
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        names.sort_by(|left, right| collator.compare(left, right));
        names
    }

    #[test]
    fn accents_sort_next_to_their_base_letter() {
        assert_eq!(
            sorted("sk", &["Duna", "Čisté Tvary", "Cyan"]),
            ["Cyan", "Čisté Tvary", "Duna"]
        );
    }

    #[test]
    fn locale_tailorings_are_applied() {
        assert_eq!(
            sorted("sk", &["Ihla", "Chata", "Hora"]),
            ["Hora", "Chata", "Ihla"]
        );
        assert_eq!(
            sorted("en", &["Ihla", "Chata", "Hora"]),
            ["Chata", "Hora", "Ihla"]
        );
    }

    #[test]
    fn case_does_not_affect_ordering() {
        assert_eq!(
            sorted("en", &["beta", "Delta", "ALPHA"]),
            ["ALPHA", "beta", "Delta"]
        );
    }

    #[tokio::test]
    async fn changed_locale_rebuilds_the_sort_indexes() {
        let db = crate::test_support::TestDatabase::new("sort-collation-locale").await;
        let pool = db.pool();

        sqlx::query(
            "INSERT INTO artist (id, name, name_sortable)
             VALUES (1, 'Čata', 'Čata'), (2, 'Zima', 'Zima')",
        )
        .execute(pool)
        .await
        .unwrap();

        super::ensure_sort_indexes_for_locale(pool, "sk")
            .await
            .unwrap();

        let recorded: String =
            sqlx::query_scalar("SELECT locale FROM sort_collation_state WHERE id = 1")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(recorded, "sk");

        // the rebuilt index is still usable and ordered by the collation
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM artist INDEXED BY idx_artist_name_sortable
             ORDER BY name_sortable COLLATE HB_SORT",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(names, ["Čata", "Zima"].map(str::to_string));

        db.close().await;
    }
}
