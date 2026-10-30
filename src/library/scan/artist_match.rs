use rustc_hash::FxHashMap;
use sqlx::SqliteConnection;

pub fn token_key(value: &str) -> String {
    let lowered = value.to_lowercase();
    let mut tokens: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

struct ArtistEntry {
    id: i64,
    name: String,
    key: String,
}

pub struct ArtistMatcher {
    artists: Option<FxHashMap<i64, ArtistEntry>>,
    by_key: FxHashMap<String, i64>,
}

impl ArtistMatcher {
    pub fn new() -> Self {
        Self {
            artists: None,
            by_key: FxHashMap::default(),
        }
    }

    pub fn clear(&mut self) {
        self.artists = None;
        self.by_key.clear();
    }

    pub fn evict(&mut self, artist_id: i64) {
        if let Some(artists) = &mut self.artists {
            artists.remove(&artist_id);
        }
        self.by_key.retain(|_, id| *id != artist_id);
    }

    async fn load(&mut self, conn: &mut SqliteConnection) -> anyhow::Result<()> {
        if self.artists.is_some() {
            return Ok(());
        }

        let rows: Vec<(i64, String, String)> =
            sqlx::query_as(include_str!("../../../queries/scan/list_artists.sql"))
                .fetch_all(&mut *conn)
                .await?;

        let mut artists = FxHashMap::default();
        // query orders by id - lowest id wins a duplicate key
        let mut by_key = FxHashMap::default();
        for (id, name, sort) in rows {
            let entry = ArtistEntry {
                id,
                name,
                key: token_key(&sort),
            };
            by_key.entry(entry.key.clone()).or_insert(entry.id);
            artists.insert(id, entry);
        }
        self.by_key = by_key;
        self.artists = Some(artists);
        Ok(())
    }

    pub async fn resolve(
        &mut self,
        conn: &mut SqliteConnection,
        name: &str,
        sort_name: Option<&str>,
    ) -> anyhow::Result<i64> {
        self.load(&mut *conn).await?;

        let explicit_sort = sort_name.and_then(|s| (!s.trim().is_empty()).then_some(s));
        let sort = explicit_sort.unwrap_or(name);
        let key = token_key(sort);
        let Some(&artist_id) = self.by_key.get(&key) else {
            return self
                .create(conn, name, sort, key, explicit_sort.is_some())
                .await;
        };

        // prefer names matching the sort key - ties keep the existing row so order doesn't matter
        let entry = self
            .artists
            .as_mut()
            .unwrap()
            .get_mut(&artist_id)
            .expect("artist matcher key should point to a loaded artist");
        let is_canonical = |name: &str| token_key(name) == entry.key;
        if is_canonical(name) && !is_canonical(&entry.name) {
            let updated = sqlx::query(include_str!("../../../queries/scan/update_artist_name.sql"))
                .bind(name)
                .bind(entry.id)
                .execute(&mut *conn)
                .await?;
            // another artist already has this name - ignore the rename
            if updated.rows_affected() > 0 {
                entry.name = name.to_string();
            }
        }

        Ok(artist_id)
    }

    async fn create(
        &mut self,
        conn: &mut SqliteConnection,
        name: &str,
        sort_name: &str,
        key: String,
        explicit_sort: bool,
    ) -> anyhow::Result<i64> {
        let result: Result<(i64,), sqlx::Error> =
            sqlx::query_as(include_str!("../../../queries/scan/create_artist.sql"))
                .bind(name)
                .bind(sort_name)
                .fetch_one(&mut *conn)
                .await;

        let (id, stored_name, stored_key) = match result {
            Ok((id,)) => (id, name.to_string(), key),
            // name is taken by an artist with a different key - use that row
            Err(sqlx::Error::RowNotFound) => {
                let (id, stored_name, stored_sort): (i64, String, String) =
                    sqlx::query_as(include_str!("../../../queries/scan/get_artist_id.sql"))
                        .bind(name)
                        .fetch_one(&mut *conn)
                        .await?;
                let existing_key = token_key(&stored_sort);
                // take a free sort - explicit tags replace old ones, defaults upgrade so aliases merge
                let can_take_sort = key != existing_key && !self.by_key.contains_key(&key);
                let upgrade = can_take_sort && (explicit_sort || stored_sort == stored_name);
                if upgrade {
                    sqlx::query(include_str!("../../../queries/scan/update_artist_sort.sql"))
                        .bind(sort_name)
                        .bind(id)
                        .execute(&mut *conn)
                        .await?;
                    (id, stored_name, key)
                } else {
                    (id, stored_name, existing_key)
                }
            }
            Err(e) => return Err(e.into()),
        };

        if let Some(entry) = self.artists.as_mut().unwrap().get_mut(&id) {
            let old_key = std::mem::replace(&mut entry.key, stored_key.clone());
            entry.name = stored_name;
            if old_key != stored_key {
                // duplicate keys from old databases map to the lowest id - only drop our own entry
                if self.by_key.get(&old_key) == Some(&id) {
                    self.by_key.remove(&old_key);
                }
                self.by_key.insert(stored_key, id);
            }
            return Ok(id);
        }
        self.by_key.insert(stored_key.clone(), id);
        self.artists.as_mut().unwrap().insert(
            id,
            ArtistEntry {
                id,
                name: stored_name,
                key: stored_key,
            },
        );
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_key_sorts_and_normalizes() {
        assert_eq!(token_key("Rundgren, Todd"), "rundgren todd");
        assert_eq!(token_key("Todd Rundgren"), "rundgren todd");
        assert_eq!(token_key("TR-i"), "i tr");
    }

    #[tokio::test]
    async fn load_resolves_duplicate_sort_keys_to_lowest_id() {
        let (_dir, pool) = crate::test_support::create_test_pool("matcher-dup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // an old database can have two artists sharing one token key
        sqlx::query("INSERT INTO artist (name, name_sortable) VALUES ('Alpha', 'Shared Sort')")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO artist (name, name_sortable) VALUES ('Beta', 'Shared Sort')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let (lowest,): (i64,) = sqlx::query_as("SELECT MIN(id) FROM artist")
            .fetch_one(&mut *conn)
            .await
            .unwrap();

        let mut matcher = ArtistMatcher::new();
        let id = matcher
            .resolve(&mut conn, "Beta", Some("Shared Sort"))
            .await
            .unwrap();
        assert_eq!(id, lowest);
    }

    #[tokio::test]
    async fn create_resolve_and_evict_keep_artist_cache_consistent() {
        let (_dir, pool) = crate::test_support::create_test_pool("matcher-cache-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let mut matcher = ArtistMatcher::new();

        let first = matcher
            .resolve(&mut conn, "First Artist", None)
            .await
            .unwrap();
        let second = matcher
            .resolve(&mut conn, "Second Artist", None)
            .await
            .unwrap();
        assert_ne!(first, second);

        matcher.evict(first);
        assert_eq!(
            matcher
                .resolve(&mut conn, "Second Artist", None)
                .await
                .unwrap(),
            second
        );
        assert_eq!(
            matcher
                .resolve(&mut conn, "First Artist", None)
                .await
                .unwrap(),
            first
        );
    }
}
