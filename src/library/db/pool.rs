use std::path::Path;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tracing::debug;

pub async fn create_pool(path: impl AsRef<Path>) -> sqlx::Result<SqlitePool> {
    debug!("Creating database pool at {:?}", path.as_ref());
    let options = SqliteConnectOptions::new()
        .filename(path)
        .synchronous(SqliteSynchronous::Normal)
        .journal_mode(SqliteJournalMode::Wal)
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

    Ok(pool)
}
