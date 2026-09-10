use rustc_hash::FxHashSet;
use sqlx::SqlitePool;

use crate::{
    library::{
        scan::{
            artist_match::ArtistMatcher,
            artwork::{ArtworkData, get_or_create_artwork},
        },
        source::SourceId,
    },
    media::metadata::Metadata,
    sources::{RemoteAlbum, RemoteArtworkMap, RemoteArtworkRef},
};

use super::{
    albums::bind_release_date,
    artist_links::{flush_album_artists, flush_track_artists, sweep_orphan_artists},
    artists::encode_artist_list,
    genre_links::{flush_album_genres, sweep_orphan_genres, sync_track_genres},
    tracks::{delete_lyrics, upsert_lyrics},
};

pub(crate) async fn begin_remote_sync(pool: &SqlitePool, source: &SourceId) -> anyhow::Result<i64> {
    if source.is_local() || source.0.is_empty() {
        anyhow::bail!("remote sync requires a non-local source");
    }

    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO library_source (id, kind) VALUES ($1, 'subsonic') \
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(&source.0)
    .execute(&mut *transaction)
    .await?;
    let generation: i64 = sqlx::query_scalar(
        "UPDATE library_source SET sync_generation = sync_generation + 1 \
         WHERE id = $1 RETURNING sync_generation",
    )
    .bind(&source.0)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(generation)
}

#[cfg(test)]
pub(crate) async fn write_remote_batch(
    pool: &SqlitePool,
    source: &SourceId,
    generation: i64,
    albums: &[RemoteAlbum],
) -> anyhow::Result<()> {
    write_remote_batch_with_artwork(
        pool,
        source,
        generation,
        albums,
        &RemoteArtworkMap::default(),
    )
    .await
}

pub(crate) async fn write_remote_batch_with_artwork(
    pool: &SqlitePool,
    source: &SourceId,
    generation: i64,
    albums: &[RemoteAlbum],
    artwork: &RemoteArtworkMap,
) -> anyhow::Result<()> {
    let mut transaction = pool.begin().await?;
    let current_generation: i64 =
        sqlx::query_scalar("SELECT sync_generation FROM library_source WHERE id = $1")
            .bind(&source.0)
            .fetch_one(&mut *transaction)
            .await?;
    if current_generation != generation {
        anyhow::bail!("a newer sync superseded this generation");
    }

    let mut pending_albums = FxHashSet::default();
    let mut pending_tracks = FxHashSet::default();
    let mut pending_genre_albums = FxHashSet::default();
    let mut matcher = ArtistMatcher::new();

    for album in albums {
        let album_artwork =
            resolve_artwork(&mut transaction, album.artwork.as_ref(), artwork).await;
        let album_id =
            upsert_remote_album(&mut transaction, source, generation, album, album_artwork).await?;
        pending_albums.insert(album_id);
        pending_genre_albums.insert(album_id);

        for track in &album.tracks {
            let track_artwork = resolve_artwork(
                &mut transaction,
                track.artwork.as_ref().or(album.artwork.as_ref()),
                artwork,
            )
            .await;
            let track_id = upsert_remote_track(
                &mut transaction,
                source,
                generation,
                album_id,
                &track.location,
                track.duration_seconds,
                &track.metadata,
                track_artwork,
            )
            .await?;
            sync_track_genres(&mut transaction, track_id, &track.metadata.genres).await?;
            if let Some(lyrics) = &track.metadata.lyrics {
                upsert_lyrics(&mut transaction, track_id, lyrics).await?;
            } else {
                delete_lyrics(&mut transaction, track_id).await?;
            }
            pending_tracks.insert(track_id);
        }
    }

    flush_album_artists(&mut transaction, &mut matcher, &mut pending_albums).await?;
    flush_track_artists(&mut transaction, &mut matcher, &mut pending_tracks).await?;
    flush_album_genres(&mut transaction, &mut pending_genre_albums).await?;
    transaction.commit().await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ArtworkUpdate {
    Keep,
    Clear,
    Set(i64),
}

async fn resolve_artwork(
    conn: &mut sqlx::SqliteConnection,
    reference: Option<&RemoteArtworkRef>,
    artwork: &RemoteArtworkMap,
) -> ArtworkUpdate {
    let Some(reference) = reference else {
        return ArtworkUpdate::Clear;
    };
    let Some(data) = artwork.get(&reference.location) else {
        return ArtworkUpdate::Keep;
    };
    get_or_create_artwork(conn, data.hash as i64, Some(ArtworkData::Raw(&data.bytes)))
        .await
        .map_or(ArtworkUpdate::Keep, ArtworkUpdate::Set)
}

fn artwork_discriminator(artwork: ArtworkUpdate) -> i32 {
    match artwork {
        ArtworkUpdate::Keep => 0,
        ArtworkUpdate::Clear => 1,
        ArtworkUpdate::Set(_) => 2,
    }
}

fn artwork_id(artwork: ArtworkUpdate) -> Option<i64> {
    match artwork {
        ArtworkUpdate::Set(id) => Some(id),
        ArtworkUpdate::Keep | ArtworkUpdate::Clear => None,
    }
}

pub(crate) async fn finish_remote_sync(
    pool: &SqlitePool,
    source: &SourceId,
    generation: i64,
) -> anyhow::Result<()> {
    let mut transaction = pool.begin().await?;
    let current_generation: i64 =
        sqlx::query_scalar("SELECT sync_generation FROM library_source WHERE id = $1")
            .bind(&source.0)
            .fetch_one(&mut *transaction)
            .await?;
    if current_generation != generation {
        anyhow::bail!("a newer sync superseded this generation");
    }

    sqlx::query("DELETE FROM track WHERE source = $1 AND source_generation != $2")
        .bind(&source.0)
        .bind(generation)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "DELETE FROM album WHERE source = $1 AND id IN (\
         SELECT album_id FROM source_album WHERE source = $1 AND last_seen_generation != $2)",
    )
    .bind(&source.0)
    .bind(generation)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "UPDATE library_source SET completed_generation = $2, \
         last_sync_completed_at = CURRENT_TIMESTAMP WHERE id = $1",
    )
    .bind(&source.0)
    .bind(generation)
    .execute(&mut *transaction)
    .await?;
    sweep_orphan_artwork(&mut transaction).await?;
    transaction.commit().await?;
    sweep_orphan_artists(pool).await;
    sweep_orphan_genres(pool).await;
    Ok(())
}

pub(crate) async fn remove_remote_source(
    pool: &SqlitePool,
    source: &SourceId,
) -> anyhow::Result<()> {
    if source.is_local() {
        anyhow::bail!("the local source cannot be removed");
    }

    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM track WHERE source = $1")
        .bind(&source.0)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM album WHERE source = $1")
        .bind(&source.0)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM library_source WHERE id = $1")
        .bind(&source.0)
        .execute(&mut *transaction)
        .await?;
    sweep_orphan_artwork(&mut transaction).await?;
    transaction.commit().await?;
    sweep_orphan_artists(pool).await;
    sweep_orphan_genres(pool).await;
    Ok(())
}

async fn sweep_orphan_artwork(conn: &mut sqlx::SqliteConnection) -> anyhow::Result<()> {
    sqlx::query(include_str!(
        "../../../../queries/scan/delete_orphan_artwork.sql"
    ))
    .execute(conn)
    .await?;
    Ok(())
}

async fn upsert_remote_album(
    conn: &mut sqlx::SqliteConnection,
    source: &SourceId,
    generation: i64,
    album: &RemoteAlbum,
    artwork: ArtworkUpdate,
) -> anyhow::Result<i64> {
    let metadata = &album.metadata;
    let title = metadata
        .album
        .as_deref()
        .or(metadata.name.as_deref())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Unknown Album");
    let display_artist = metadata
        .album_artist
        .as_deref()
        .or(metadata.artist.as_deref())
        .unwrap_or("");
    let (release_date, date_precision) = bind_release_date(metadata);
    let mbid = metadata.mbid_album.as_deref().unwrap_or("none");

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT album_id FROM source_album WHERE source = $1 AND location = $2")
            .bind(&source.0)
            .bind(&album.location)
            .fetch_optional(&mut *conn)
            .await?;

    let album_id = if let Some(album_id) = existing {
        sqlx::query(
            "UPDATE album SET title = $2, title_sortable = $3, \
             artist_display_override = $4, artist_sort = COALESCE($5, artist_sort), \
             artist_sort_tag = $5, release_date = $6, date_precision = $7, label = $8, \
             catalog_number = $9, isrc = $10, mbid = $11, number_display_mode = $12, \
             artwork_id = CASE $13 WHEN 0 THEN artwork_id WHEN 1 THEN NULL ELSE $14 END \
             WHERE id = $1 AND source = $15",
        )
        .bind(album_id)
        .bind(title)
        .bind(metadata.sort_album.as_deref().unwrap_or(title))
        .bind(display_artist)
        .bind(metadata.album_artist_sort.as_deref())
        .bind(release_date)
        .bind(date_precision)
        .bind(&metadata.label)
        .bind(&metadata.catalog)
        .bind(&metadata.isrc)
        .bind(mbid)
        .bind(metadata.number_display_mode)
        .bind(artwork_discriminator(artwork))
        .bind(artwork_id(artwork))
        .bind(&source.0)
        .execute(&mut *conn)
        .await?;
        album_id
    } else {
        let album_id: i64 = sqlx::query_scalar(
            "INSERT INTO album \
             (title, title_sortable, artist_display_override, artist_sort, artist_sort_tag, \
              release_date, date_precision, label, catalog_number, isrc, mbid, \
              number_display_mode, source, artwork_id) \
             VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
        )
        .bind(title)
        .bind(metadata.sort_album.as_deref().unwrap_or(title))
        .bind(display_artist)
        .bind(metadata.album_artist_sort.as_deref())
        .bind(release_date)
        .bind(date_precision)
        .bind(&metadata.label)
        .bind(&metadata.catalog)
        .bind(&metadata.isrc)
        .bind(mbid)
        .bind(metadata.number_display_mode)
        .bind(&source.0)
        .bind(artwork_id(artwork))
        .fetch_one(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT INTO source_album \
             (source, location, album_id, last_seen_generation) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&source.0)
        .bind(&album.location)
        .bind(album_id)
        .bind(generation)
        .execute(&mut *conn)
        .await?;
        album_id
    };

    sqlx::query(
        "UPDATE source_album SET last_seen_generation = $3 \
         WHERE source = $1 AND location = $2",
    )
    .bind(&source.0)
    .bind(&album.location)
    .bind(generation)
    .execute(&mut *conn)
    .await?;
    Ok(album_id)
}

#[allow(clippy::too_many_arguments)]
async fn upsert_remote_track(
    conn: &mut sqlx::SqliteConnection,
    source: &SourceId,
    generation: i64,
    album_id: i64,
    location: &str,
    duration_seconds: u64,
    metadata: &Metadata,
    artwork: ArtworkUpdate,
) -> anyhow::Result<i64> {
    let title = metadata
        .name
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("Unknown Track");
    let (release_date, date_precision) = bind_release_date(metadata);
    let artists = encode_artist_list(&metadata.artists);
    let album_artist_keys = encode_artist_list(&metadata.album_artist_keys);
    let track_number = metadata.track_current.map(i32::try_from).transpose()?;
    let disc_number = metadata.disc_current.map(i32::try_from).transpose()?;
    let track_section = metadata.track_section.map(i32::try_from).transpose()?;
    let duration = i32::try_from(duration_seconds)?;

    let track_id: i64 = sqlx::query_scalar(
        "INSERT INTO track \
         (title, title_sortable, album_id, track_number, disc_number, duration, location, \
          artist_names, folder, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, \
          disc_subtitle, artists, artist_sort, album_artist_keys, release_date, date_precision, \
          track_section, number_display_mode_hint, source, source_generation, artwork_id) \
         VALUES \
         ($1, $2, $3, $4, $5, $6, $7, $8, NULL, $9, $10, $11, $12, $13, $14, $15, $16, \
          $17, $18, $19, $20, $21, $22, $23) \
         ON CONFLICT(source, location) DO UPDATE SET \
          title = excluded.title, title_sortable = excluded.title_sortable, \
          album_id = excluded.album_id, track_number = excluded.track_number, \
          disc_number = excluded.disc_number, duration = excluded.duration, \
          artist_names = excluded.artist_names, folder = NULL, \
          rg_track_gain = excluded.rg_track_gain, rg_track_peak = excluded.rg_track_peak, \
          rg_album_gain = excluded.rg_album_gain, rg_album_peak = excluded.rg_album_peak, \
          disc_subtitle = excluded.disc_subtitle, artists = excluded.artists, \
          artist_sort = excluded.artist_sort, album_artist_keys = excluded.album_artist_keys, \
          release_date = excluded.release_date, date_precision = excluded.date_precision, \
          track_section = excluded.track_section, \
          number_display_mode_hint = excluded.number_display_mode_hint, \
          source_generation = excluded.source_generation, \
          artwork_id = CASE $24 WHEN 0 THEN track.artwork_id ELSE excluded.artwork_id END \
         RETURNING id",
    )
    .bind(title)
    .bind(title)
    .bind(album_id)
    .bind(track_number)
    .bind(disc_number)
    .bind(duration)
    .bind(location)
    .bind(&metadata.artist)
    .bind(metadata.replaygain_track_gain)
    .bind(metadata.replaygain_track_peak)
    .bind(metadata.replaygain_album_gain)
    .bind(metadata.replaygain_album_peak)
    .bind(&metadata.disc_subtitle)
    .bind(artists)
    .bind(&metadata.artist_sort)
    .bind(album_artist_keys)
    .bind(release_date)
    .bind(date_precision)
    .bind(track_section)
    .bind(metadata.number_display_mode)
    .bind(&source.0)
    .bind(generation)
    .bind(artwork_id(artwork))
    .bind(artwork_discriminator(artwork))
    .fetch_one(&mut *conn)
    .await?;
    Ok(track_id)
}
