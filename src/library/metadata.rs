//! Shared metadata persistence; filesystem discovery and folder claims belong to
//! the local scanner, while source adapters supply normalized metadata here.
use super::types::{DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH};
use crate::{media::metadata::Metadata, sources::SourceId};
use sqlx::SqliteConnection;

pub(crate) fn encode_artist_list(values: &[String]) -> Option<String> {
    (!values.is_empty()).then(|| serde_json::to_string(values).expect("artist list serializes"))
}

pub(crate) fn bind_release_date(metadata: &Metadata) -> (Option<String>, Option<i32>) {
    if let Some(date) = metadata.date {
        return (
            Some(date.format("%Y-%m-%d").to_string()),
            Some(DATE_PRECISION_FULL_DATE),
        );
    }

    if let Some((year, month)) = metadata.year_month {
        return (
            Some(format!("{year:04}-{month:02}-01")),
            Some(DATE_PRECISION_YEAR_MONTH),
        );
    }

    if let Some(year) = metadata.year {
        return (Some(format!("{year:04}-01-01")), Some(DATE_PRECISION_YEAR));
    }

    (None, None)
}

/// Upsert without replacing the track row, preserving ID and user relationships.
/// The caller owns the transaction and subsequent normalized relationship writes.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    source: &SourceId,
    location: &str,
    folder: Option<&str>,
    album_id: Option<i64>,
    name: &str,
    length: u64,
    art_hash: Option<i64>,
    generation: i64,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        source.is_local() || folder.is_none(),
        "remote tracks cannot claim a folder"
    );
    let (release_date, date_precision) = bind_release_date(metadata);
    let artists = encode_artist_list(&metadata.artists);
    let album_artist_keys = encode_artist_list(&metadata.album_artist_keys);
    let track_number = metadata.track_current.map(i32::try_from).transpose()?;
    let disc_number = metadata.disc_current.map(i32::try_from).transpose()?;
    let track_section = metadata.track_section.map(i32::try_from).transpose()?;

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/library/write_track.sql"))
            .bind(name)
            .bind(name)
            .bind(album_id)
            .bind(track_number)
            .bind(disc_number)
            .bind(i64::try_from(length)?)
            .bind(location)
            .bind(&metadata.artist)
            .bind(folder)
            .bind(metadata.replaygain_track_gain)
            .bind(metadata.replaygain_track_peak)
            .bind(metadata.replaygain_album_gain)
            .bind(metadata.replaygain_album_peak)
            .bind(&metadata.disc_subtitle)
            .bind(artists)
            .bind(&metadata.artist_sort)
            .bind(album_artist_keys)
            .bind(art_hash)
            .bind(release_date)
            .bind(date_precision)
            .bind(track_section)
            .bind(metadata.number_display_mode)
            .bind(source)
            .bind(generation)
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok((track_id,)) => Ok(track_id),
        Err(sqlx::Error::RowNotFound) => Err(anyhow::anyhow!("create_track returned no row")),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{create_test_pool, track_metadata};

    #[tokio::test]
    async fn remote_metadata_upserts_keep_id_playlist_and_local_copy() {
        let (_dir, pool) = create_test_pool("shared-metadata-source").await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO library_source(id, kind) VALUES('server', 'subsonic')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let metadata = track_metadata("Same", "Artist", "Song", 1);
        let local = write_track(
            &mut conn,
            &metadata,
            &SourceId::local(),
            "/Music/song.flac",
            Some("/Music"),
            None,
            "Song",
            180,
            None,
            0,
        )
        .await
        .unwrap();
        let remote = write_track(
            &mut conn,
            &metadata,
            &SourceId::new("server"),
            "/Music/song.flac",
            None,
            None,
            "Remote song",
            180,
            None,
            1,
        )
        .await
        .unwrap();
        assert_ne!(local, remote);
        sqlx::query("INSERT INTO playlist_item(playlist_id,track_id,position) SELECT id,$1,0 FROM playlist WHERE name='Liked Songs'")
            .bind(remote).execute(&mut *conn).await.unwrap();
        let refreshed = write_track(
            &mut conn,
            &metadata,
            &SourceId::new("server"),
            "/Music/song.flac",
            None,
            None,
            "Updated title",
            181,
            None,
            2,
        )
        .await
        .unwrap();
        assert_eq!(remote, refreshed);
        let (title, folder, generation): (String, Option<String>, i64) =
            sqlx::query_as("SELECT title,folder,sync_generation FROM track WHERE id=$1")
                .bind(remote)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(title, "Updated title");
        assert_eq!(folder, None);
        assert_eq!(generation, 2);
        let membership: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_item WHERE track_id=$1")
                .bind(remote)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(membership, 1);
        let local_title: String = sqlx::query_scalar("SELECT title FROM track WHERE id=$1")
            .bind(local)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(local_title, "Song");
    }
}
