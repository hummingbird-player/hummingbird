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
    artists: Option<Vec<ArtistEntry>>,
    by_key: FxHashMap<String, usize>,
}

impl Default for ArtistMatcher {
    fn default() -> Self {
        Self::new()
    }
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
        let Some(artists) = &mut self.artists else {
            return;
        };
        let Some(pos) = artists.iter().position(|entry| entry.id == artist_id) else {
            return;
        };
        let entry = artists.remove(pos);
        self.by_key.remove(&entry.key);
        // indices above the removed entry shift down
        for index in self.by_key.values_mut() {
            if *index > pos {
                *index -= 1;
            }
        }
    }

    async fn load(&mut self, conn: &mut SqliteConnection) -> anyhow::Result<()> {
        if self.artists.is_some() {
            return Ok(());
        }

        let rows: Vec<(i64, String, String)> =
            sqlx::query_as(include_str!("../../../queries/scan/list_artists.sql"))
                .fetch_all(&mut *conn)
                .await?;

        let artists: Vec<ArtistEntry> = rows
            .into_iter()
            .map(|(id, name, sort)| ArtistEntry {
                id,
                name,
                key: token_key(&sort),
            })
            .collect();
        // the query orders by id, the lowest id wins a duplicate key
        let mut by_key = FxHashMap::default();
        for (i, entry) in artists.iter().enumerate() {
            by_key.entry(entry.key.clone()).or_insert(i);
        }
        self.by_key = by_key;
        self.artists = Some(artists);
        Ok(())
    }

    /// Resolve a raw artist string to an artist id, creating or renaming artists as needed.
    pub async fn resolve(
        &mut self,
        conn: &mut SqliteConnection,
        name: &str,
        sort_name: Option<&str>,
    ) -> anyhow::Result<i64> {
        self.load(&mut *conn).await?;

        let sort = sort_name
            .and_then(|s| (!s.trim().is_empty()).then_some(s))
            .unwrap_or(name);
        let key = token_key(sort);
        let Some(&index) = self.by_key.get(&key) else {
            return self.create(conn, name, sort, key).await;
        };

        // a name matching the sort key displaces an alias; ties keep the incumbent so the
        // result doesn't depend on scan order
        let entry = &mut self.artists.as_mut().unwrap()[index];
        let is_canonical = |name: &str| token_key(name) == entry.key;
        if is_canonical(name) && !is_canonical(&entry.name) {
            let updated = sqlx::query(include_str!("../../../queries/scan/update_artist_name.sql"))
                .bind(name)
                .bind(entry.id)
                .execute(&mut *conn)
                .await?;
            // another artist can hold the name already, then the rename is ignored
            if updated.rows_affected() > 0 {
                entry.name = name.to_string();
            }
        }

        Ok(entry.id)
    }

    async fn create(
        &mut self,
        conn: &mut SqliteConnection,
        name: &str,
        sort_name: &str,
        key: String,
    ) -> anyhow::Result<i64> {
        let result: Result<(i64,), sqlx::Error> =
            sqlx::query_as(include_str!("../../../queries/scan/create_artist.sql"))
                .bind(name)
                .bind(sort_name)
                .fetch_one(&mut *conn)
                .await;

        let (id, name, key) = match result {
            Ok((id,)) => (id, name.to_string(), key),
            // the name is taken by an artist with a different key, adopt that row
            Err(sqlx::Error::RowNotFound) => {
                let (id, name, sort): (i64, String, String) =
                    sqlx::query_as(include_str!("../../../queries/scan/get_artist_id.sql"))
                        .bind(name)
                        .fetch_one(&mut *conn)
                        .await?;
                let existing_key = token_key(&sort);
                // a row whose sort defaulted to its name takes the incoming tag so later
                // aliases merge, unless another artist holds that key
                let upgrade =
                    sort == name && key != existing_key && !self.by_key.contains_key(&key);
                if upgrade {
                    sqlx::query(include_str!("../../../queries/scan/update_artist_sort.sql"))
                        .bind(sort_name)
                        .bind(id)
                        .execute(&mut *conn)
                        .await?;
                    (id, name, key)
                } else {
                    (id, name, existing_key)
                }
            }
            Err(e) => return Err(e.into()),
        };

        let artists = self.artists.as_mut().unwrap();
        // a matching entry can exist when the adopted row was already loaded
        if let Some(&index) = self.by_key.get(&key).filter(|&&i| artists[i].id == id) {
            artists[index].name = name;
            return Ok(artists[index].id);
        }
        // the adopted row is already loaded: update it in place, an incoming sort tag can
        // shift its key and a stale key must not outlive the row when it is evicted later
        if let Some(index) = artists.iter().position(|entry| entry.id == id) {
            let entry = &mut artists[index];
            entry.name = name;
            if entry.key != key {
                self.by_key.remove(&entry.key);
                entry.key = key.clone();
                self.by_key.insert(key, index);
            }
            return Ok(id);
        }
        self.by_key.insert(key.clone(), artists.len());
        artists.push(ArtistEntry { id, name, key });
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

        // a legacy database can hold two artists sharing one token key
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
}
