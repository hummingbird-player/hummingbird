//! Host-side remote catalog persistence. Backends never receive these connections.
use super::{
    SourceId,
    backend::{ReleaseDate, RemoteAlbum, RemoteArtist, RemoteTrack},
};
use crate::{
    library::{
        metadata::{bind_release_date, write_track},
        scan::{
            artist_match::ArtistMatcher,
            database::{recompute_album_genres, sync_track_genres},
        },
    },
    media::metadata::Metadata,
};
use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::SqliteConnection;
use std::collections::BTreeSet;

fn apply_date(metadata: &mut Metadata, date: Option<&ReleaseDate>) -> anyhow::Result<()> {
    if let Some(date) = date {
        anyhow::ensure!(
            (1..=9999).contains(&date.year),
            "invalid remote release year"
        );
        let month = date.month.unwrap_or(1) as u32;
        let day = date.day.unwrap_or(1) as u32;
        let value = NaiveDate::from_ymd_opt(date.year, month, day)
            .ok_or_else(|| anyhow::anyhow!("invalid remote release date"))?;
        anyhow::ensure!(
            date.day.is_none() || date.month.is_some(),
            "day requires a month"
        );
        if date.day.is_some() {
            metadata.date = Some(Utc.from_utc_datetime(&value.and_hms_opt(0, 0, 0).unwrap()));
        } else if date.month.is_some() {
            metadata.year_month = Some((date.year as u16, month as u8));
        } else {
            metadata.year = Some(date.year as u16);
        }
    }
    Ok(())
}
fn validate_id(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.is_empty() && value.len() <= 4096,
        "invalid remote identity"
    );
    Ok(())
}

pub(crate) struct CatalogWriter {
    source: SourceId,
    scope: String,
    generation: i64,
    matcher: ArtistMatcher,
    pending_albums: BTreeSet<i64>,
}
impl CatalogWriter {
    pub fn new(source: SourceId, scope: String, generation: i64) -> Self {
        Self {
            source,
            scope,
            generation,
            matcher: ArtistMatcher::new(),
            pending_albums: BTreeSet::new(),
        }
    }
    pub async fn artist(
        &mut self,
        conn: &mut SqliteConnection,
        artist: &RemoteArtist,
    ) -> anyhow::Result<i64> {
        anyhow::ensure!(artist.name.len() <= 65536, "artist name exceeds limit");
        if !artist.id.is_empty() {
            validate_id(&artist.id)?;
            if let Some(id) = sqlx::query_scalar(
                "SELECT artist_id FROM remote_artist WHERE source=$1 AND remote_id=$2",
            )
            .bind(&self.source)
            .bind(&artist.id)
            .fetch_optional(&mut *conn)
            .await?
            {
                return Ok(id);
            }
        }
        let name = if artist.name.trim().is_empty() {
            "Unknown Artist"
        } else {
            artist.name.trim()
        };
        let id = self
            .matcher
            .resolve_preserving_metadata(conn, name, artist.sort_name.as_deref())
            .await?;
        if !artist.id.is_empty() {
            sqlx::query("INSERT INTO remote_artist(source,remote_id,artist_id) VALUES($1,$2,$3) ON CONFLICT(source,remote_id) DO NOTHING")
                .bind(&self.source).bind(&artist.id).bind(id).execute(&mut *conn).await?;
        }
        Ok(id)
    }
    pub async fn album(
        &mut self,
        conn: &mut SqliteConnection,
        album: &RemoteAlbum,
        supplemental: bool,
    ) -> anyhow::Result<i64> {
        validate_id(&album.id)?;
        if supplemental {
            if let Some(id) = sqlx::query_scalar("SELECT album_id FROM remote_album WHERE source=$1 AND remote_id=$2 AND sync_generation=$3").bind(&self.source).bind(&album.id).bind(self.generation).fetch_optional(&mut *conn).await? { return Ok(id); }
        }
        anyhow::ensure!(album.title.len() <= 65536, "album title exceeds limit");
        let mut metadata = Metadata::default();
        apply_date(&mut metadata, album.release_date.as_ref())?;
        let (date, precision) = bind_release_date(&metadata);
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT album_id FROM remote_album WHERE source=$1 AND remote_id=$2",
        )
        .bind(&self.source)
        .bind(&album.id)
        .fetch_optional(&mut *conn)
        .await?;
        let title = if album.title.is_empty() {
            &album.id
        } else {
            &album.title
        };
        let id = match existing {
            Some(id) => {
                sqlx::query("UPDATE album SET title=COALESCE(NULLIF($1,''),title),title_sortable=CASE WHEN $11 IS NULL AND title=$1 THEN title_sortable ELSE COALESCE(NULLIF($2,''),title_sortable) END,artist_display_override=COALESCE($3,artist_display_override),release_date=COALESCE($4,release_date),date_precision=COALESCE($5,date_precision),mbid=COALESCE($6,mbid),label=COALESCE($7,label),catalog_number=COALESCE($8,catalog_number) WHERE id=$9 AND source=$10")
                    .bind(&album.title).bind(album.sort_title.as_ref().unwrap_or(&album.title)).bind(&album.artist_display)
                    .bind(&date).bind(precision).bind(&album.musicbrainz_id).bind(&album.label).bind(&album.catalog_number)
                    .bind(id).bind(&self.source).bind(&album.sort_title).execute(&mut *conn).await?;
                id
            }
            None => {
                let id: i64 = sqlx::query_scalar("INSERT INTO album(source,title,title_sortable,artist_display_override,release_date,date_precision,mbid,label,catalog_number) VALUES($1,$2,$3,COALESCE($4,''),$5,$6,COALESCE($7,'none'),$8,$9) RETURNING id")
                    .bind(&self.source).bind(title).bind(album.sort_title.as_ref().unwrap_or(title)).bind(&album.artist_display)
                    .bind(&date).bind(precision).bind(&album.musicbrainz_id).bind(&album.label).bind(&album.catalog_number)
                    .fetch_one(&mut *conn).await?;
                sqlx::query("INSERT INTO remote_album(source,remote_id,album_id) VALUES($1,$2,$3)")
                    .bind(&self.source)
                    .bind(&album.id)
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                id
            }
        };
        sqlx::query("UPDATE remote_album SET artwork_locator=COALESCE($1,artwork_locator),sync_generation=$4 WHERE source=$2 AND remote_id=$3")
            .bind(&album.artwork).bind(&self.source).bind(&album.id).bind(self.generation).execute(&mut *conn).await?;
        if album.artists.is_some() || existing.is_none() {
            let fallback = vec![RemoteArtist {
                name: album
                    .artist_display
                    .clone()
                    .unwrap_or_else(|| "Unknown Artist".into()),
                ..Default::default()
            }];
            let artists = album
                .artists
                .as_ref()
                .filter(|artists| !artists.is_empty())
                .unwrap_or(&fallback);
            anyhow::ensure!(artists.len() <= 128, "too many album artists");
            let mut ids = Vec::new();
            for artist in artists {
                let artist_id = self.artist(conn, artist).await?;
                if !ids.contains(&artist_id) {
                    ids.push(artist_id);
                }
                sqlx::query("INSERT OR IGNORE INTO album_artist(album_id,artist_id) VALUES($1,$2)")
                    .bind(id)
                    .bind(artist_id)
                    .execute(&mut *conn)
                    .await?;
            }
            let existing: Vec<i64> =
                sqlx::query_scalar("SELECT artist_id FROM album_artist WHERE album_id=$1")
                    .bind(id)
                    .fetch_all(&mut *conn)
                    .await?;
            for artist_id in existing {
                if !ids.contains(&artist_id) {
                    sqlx::query("DELETE FROM album_artist WHERE album_id=$1 AND artist_id=$2")
                        .bind(id)
                        .bind(artist_id)
                        .execute(&mut *conn)
                        .await?;
                    self.matcher.evict(artist_id);
                }
            }
        }
        sqlx::query(include_str!(
            "../../queries/scan/update_album_artist_sort.sql"
        ))
        .bind(id)
        .execute(&mut *conn)
        .await?;
        Ok(id)
    }
    /// Aggregate once per committed batch, not once for every song on an album.
    pub async fn flush(&mut self, conn: &mut SqliteConnection) -> anyhow::Result<()> {
        for id in &self.pending_albums {
            recompute_album_genres(conn, *id).await?;
        }
        self.pending_albums.clear();
        Ok(())
    }
    pub async fn track(
        &mut self,
        conn: &mut SqliteConnection,
        track: &RemoteTrack,
        supplemental: bool,
    ) -> anyhow::Result<i64> {
        validate_id(&track.id)?;
        if supplemental {
            if let Some(id) = sqlx::query_scalar(
                "SELECT id FROM track WHERE source=$1 AND location=$2 AND sync_generation=$3",
            )
            .bind(&self.source)
            .bind(&track.id)
            .bind(self.generation)
            .fetch_optional(&mut *conn)
            .await?
            {
                return Ok(id);
            }
        }
        anyhow::ensure!(track.title.len() <= 65536, "track title exceeds limit");
        let existing: Option<(i64, Option<i64>, Option<i64>)> =
            sqlx::query_as("SELECT track.id,album_id,source_track.duration_ms FROM track LEFT JOIN source_track ON source_track.track_id=track.id WHERE source=$1 AND location=$2")
                .bind(&self.source)
                .bind(&track.id)
                .fetch_optional(&mut *conn)
                .await?;
        let album_id = if let Some(remote_id) = &track.album_id {
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT album_id FROM remote_album WHERE source=$1 AND remote_id=$2",
            )
            .bind(&self.source)
            .bind(remote_id)
            .fetch_optional(&mut *conn)
            .await?;
            Some(id.ok_or_else(|| anyhow::anyhow!("track album was not supplied by the catalog"))?)
        } else if track.album_known {
            None
        } else {
            existing.and_then(|(_, album, _)| album)
        };
        let mut metadata = Metadata {
            name: Some(track.title.clone()),
            artist: track.artist_display.clone(),
            track_current: track.track_number.map(u64::from),
            disc_current: track.disc_number.map(u64::from),
            disc_subtitle: track.disc_subtitle.clone(),
            replaygain_track_gain: track.replay_gain.track_gain,
            replaygain_track_peak: track.replay_gain.track_peak,
            replaygain_album_gain: track.replay_gain.album_gain,
            replaygain_album_peak: track.replay_gain.album_peak,
            ..Default::default()
        };
        for gain in [
            metadata.replaygain_track_gain,
            metadata.replaygain_track_peak,
            metadata.replaygain_album_gain,
            metadata.replaygain_album_peak,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(gain.is_finite(), "non-finite replay gain");
        }
        apply_date(&mut metadata, track.release_date.as_ref())?;
        let fallback_artists = vec![RemoteArtist {
            name: track
                .artist_display
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Unknown Artist".into()),
            ..Default::default()
        }];
        let artists_to_write = track
            .artists
            .as_ref()
            .or_else(|| existing.is_none().then_some(&fallback_artists));

        if let Some(artists) = artists_to_write {
            anyhow::ensure!(artists.len() <= 128, "too many track artists");
            metadata.artists = artists.iter().map(|artist| artist.name.clone()).collect();
            if metadata.artists.is_empty() {
                metadata.artists.push("Unknown Artist".into());
            }
            if metadata.artist.is_none() {
                metadata.artist = Some(metadata.artists.join(", "));
            }
        }
        let duration_ms = track
            .duration_ms
            .or_else(|| {
                existing
                    .and_then(|(_, _, duration)| duration)
                    .and_then(|duration| u64::try_from(duration).ok())
            })
            .unwrap_or(0);
        let previous_title: Option<String> = if track.title.is_empty() && existing.is_some() {
            sqlx::query_scalar("SELECT title FROM track WHERE id=$1")
                .bind(existing.map(|(id, _, _)| id))
                .fetch_optional(&mut *conn)
                .await?
        } else {
            None
        };
        let title = if track.title.is_empty() {
            previous_title.as_deref().unwrap_or(&track.id)
        } else {
            &track.title
        };
        let id = write_track(
            conn,
            &metadata,
            &self.source,
            &track.id,
            None,
            album_id,
            title,
            duration_ms / 1000,
            None,
            self.generation,
        )
        .await?;
        if let Some(sort) = &track.sort_title {
            sqlx::query("UPDATE track SET title_sortable=$1 WHERE id=$2")
                .bind(sort)
                .bind(id)
                .execute(&mut *conn)
                .await?;
        }
        if let Some(genres) = &track.genres {
            anyhow::ensure!(
                genres.len() <= 128 && genres.iter().all(|genre| genre.len() <= 4096),
                "genre metadata exceeds limit"
            );
            sync_track_genres(conn, id, genres).await?;
        }
        if let Some(artists) = artists_to_write {
            let fallback = vec![RemoteArtist {
                name: "Unknown Artist".into(),
                ..Default::default()
            }];
            let artists = if artists.is_empty() {
                &fallback
            } else {
                artists
            };
            let mut ids = Vec::new();
            for artist in artists {
                let artist_id = self.artist(conn, artist).await?;
                if !ids.contains(&artist_id) {
                    ids.push(artist_id);
                }
                sqlx::query("INSERT OR IGNORE INTO track_artist(track_id,artist_id) VALUES($1,$2)")
                    .bind(id)
                    .bind(artist_id)
                    .execute(&mut *conn)
                    .await?;
            }
            let previous: Vec<i64> =
                sqlx::query_scalar("SELECT artist_id FROM track_artist WHERE track_id=$1")
                    .bind(id)
                    .fetch_all(&mut *conn)
                    .await?;
            for artist_id in previous {
                if !ids.contains(&artist_id) {
                    sqlx::query("DELETE FROM track_artist WHERE track_id=$1 AND artist_id=$2")
                        .bind(id)
                        .bind(artist_id)
                        .execute(&mut *conn)
                        .await?;
                    self.matcher.evict(artist_id);
                }
            }
        }
        if let Some(lyrics) = &track.lyrics {
            anyhow::ensure!(lyrics.len() <= 1024 * 1024, "lyrics exceed limit");
            sqlx::query(include_str!("../../queries/scan/upsert_lyrics.sql"))
                .bind(id)
                .bind(lyrics)
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query("INSERT INTO source_track(track_id,scope,duration_ms,artwork_locator,content_revision,original_format,original_bitrate_kbps,musicbrainz_id,starred_baseline,rating_baseline) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(track_id) DO UPDATE SET scope=EXCLUDED.scope,duration_ms=EXCLUDED.duration_ms,artwork_locator=COALESCE(EXCLUDED.artwork_locator,source_track.artwork_locator),content_revision=COALESCE(EXCLUDED.content_revision,source_track.content_revision),original_format=COALESCE(EXCLUDED.original_format,source_track.original_format),original_bitrate_kbps=COALESCE(EXCLUDED.original_bitrate_kbps,source_track.original_bitrate_kbps),musicbrainz_id=COALESCE(EXCLUDED.musicbrainz_id,source_track.musicbrainz_id),starred_baseline=COALESCE(EXCLUDED.starred_baseline,source_track.starred_baseline),rating_baseline=COALESCE(EXCLUDED.rating_baseline,source_track.rating_baseline)")
            .bind(id).bind(&self.scope).bind(i64::try_from(duration_ms)?).bind(&track.artwork).bind(&track.content_revision)
            .bind(&track.original_format).bind(track.original_bitrate_kbps.map(i64::from)).bind(&track.musicbrainz_id).bind(track.starred).bind(track.rating.map(i64::from))
            .execute(&mut *conn).await?;
        // Import initial user state; subsequent remote reads cannot overwrite local
        // explicit changes. Write-back reconciliation tracks its own baseline.
        if existing.is_none() {
            if track.starred == Some(true) {
                sqlx::query("INSERT OR IGNORE INTO playlist_item(playlist_id,track_id,position) SELECT id,$1,COALESCE((SELECT MAX(position)+1 FROM playlist_item WHERE playlist_id=playlist.id),0) FROM playlist WHERE type=1 AND name='Liked Songs'")
                    .bind(id).execute(&mut *conn).await?;
            }
            if let Some(rating) = track.rating {
                sqlx::query("UPDATE track SET rating=$1 WHERE id=$2")
                    .bind(i64::from(rating))
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
            }
        }
        self.pending_albums.extend(
            [existing.and_then(|(_, id, _)| id), album_id]
                .into_iter()
                .flatten(),
        );
        Ok(id)
    }
}
