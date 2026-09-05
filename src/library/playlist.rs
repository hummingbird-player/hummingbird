use crate::sources::{TrackRef, playlist_reference};
use std::{ffi::OsStr, path::Path};

use anyhow::Context as _;
use cntp_i18n::tr;
use futures::{StreamExt as _, TryFutureExt as _, TryStreamExt as _};
use gpui::{App, PathPromptOptions};
use sqlx::{Sqlite, SqlitePool};
use tokio::{fs::File, io::BufWriter};
use tracing::{Instrument as _, debug_span, error, info, warn};

use crate::ui::{
    app::Pool,
    models::{Models, PlaylistEvent},
};

#[cfg(windows)]
const LINE_ENDING: &str = "\r\n";
#[cfg(not(windows))]
const LINE_ENDING: &str = "\n";

#[derive(sqlx::FromRow)]
struct PlaylistEntry {
    #[sqlx(flatten)]
    reference: TrackRef,
    duration: u32,
    track_artist_names: String,
    artist_name: String,
    track_title: String,
    album_title: String,
}

async fn write_m3u(mut w: BufWriter<File>, pool: &SqlitePool, pl_id: i64) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    w.write_all(b"#EXTM3U").await?;

    {
        let query = include_str!("../../queries/playlist/list_tracks_for_export.sql");
        let mut entries = sqlx::query_as(query).bind(pl_id).fetch(pool);
        let mut buf = vec![];
        while let Some(PlaylistEntry {
            reference,
            duration,
            track_artist_names,
            artist_name,
            track_title,
            album_title,
        }) = entries.try_next().await?
        {
            let location = playlist_reference::encode(&reference)?;
            // Server tags are untrusted text, not additional M3U records.
            let track_artist_names = track_artist_names.replace(['\r', '\n'], " ");
            let artist_name = artist_name.replace(['\r', '\n'], " ");
            let track_title = track_title.replace(['\r', '\n'], " ");
            let album_title = album_title.replace(['\r', '\n'], " ");
            use std::io::Write as _;
            write!(
                &mut buf,
                "{LINE_ENDING}\
                #EXTINF:{duration},{track_artist_names} - {track_title}{LINE_ENDING}\
                #EXTALB:{album_title}{LINE_ENDING}\
                #EXTART:{artist_name}{LINE_ENDING}\
                {location}{LINE_ENDING}",
            )?;

            w.write_all(&buf).await?;
            buf.clear();
        }
    }

    w.shutdown().await?;
    Ok(())
}

pub fn export_playlist(cx: &App, pl_id: i64, playlist_name: &str) -> anyhow::Result<()> {
    let path_future = cx.prompt_for_new_path(
        directories::UserDirs::new()
            .context("Failed to get user directories")?
            .document_dir()
            .context("Failed to get documents directory")?,
        Some(&format!("{playlist_name}.m3u8")),
    );
    let pool = cx.global::<Pool>().0.clone();

    crate::RUNTIME.spawn(async move {
        let path = match path_future.err_into().await.flatten() {
            Ok(Some(path)) => path,
            Ok(None) => return info!("Playlist export cancelled by user"),
            Err(err) => return error!(?err, "Failed to prompt for path: {err}"),
        };

        if let Err(err) = File::create(&path)
            .err_into()
            .map_ok(BufWriter::new)
            .and_then(|f| write_m3u(f, &pool, pl_id))
            .instrument(debug_span!("export_playlist", pl_id, path = %path.display()))
            .await
        {
            error!(?err, "Failed writing playlist to {}: {err}", path.display());
        }
    });

    Ok(())
}

#[derive(Debug, Default)]
struct M3UEntry {
    duration: Option<u32>,
    track_artist_names: Option<String>,
    track_title: Option<String>,
    album_title: Option<String>,
    artist_name: Option<String>,
    location: String,
}

fn parse_m3u(file: File) -> impl futures::Stream<Item = anyhow::Result<M3UEntry>> {
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio_stream::wrappers::LinesStream;

    let lines = LinesStream::new(BufReader::new(file).lines()).enumerate();
    futures::stream::try_unfold(lines, async |mut lines| {
        let mut current_entry = M3UEntry::default();
        while let Some((line, res)) = lines.next().await {
            let txt = res.inspect_err(|err| error!(%line, ?err, "IO error: {err}"))?;
            if let Some(line) = txt.strip_prefix("#EXTINF:") {
                let Some((dur, info)) = line.split_once(',') else {
                    continue;
                };

                match dur.parse() {
                    Ok(dur) => current_entry.duration = Some(dur),
                    Err(err) => warn!(%line, ?err, "Failed to parse track duration: {err}"),
                }

                if let Some((artist, title)) = info.split_once(['-', ':', '\u{2013}']) {
                    current_entry.track_artist_names = Some(artist.trim().into());
                    current_entry.track_title = Some(title.trim().into());
                } else {
                    current_entry.track_title = Some(info.trim().into());
                }
            } else if let Some(album_title) = txt.strip_prefix("#EXTALB:") {
                current_entry.album_title = Some(album_title.into());
            } else if let Some(artist_name) = txt.strip_prefix("#EXTART:") {
                current_entry.artist_name = Some(artist_name.into());
            } else if !txt.starts_with('#') && !txt.is_empty() {
                current_entry.location = txt.into();
                tracing::debug!("Parsed track: {current_entry:?}");
                return Ok(Some((current_entry, lines)));
            } else {
                tracing::debug!(%line, "Ignoring line: '{txt}'");
            }
        }

        Ok(None)
    })
}

pub fn import_playlist(cx: &App, playlist_id: i64) {
    let path_future = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(tr!("SELECT_M3U", "Select a M3U file...").into()),
    });

    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move {
            let Some(path) = path_future.await??.and_then(|v| v.into_iter().next()) else {
                info!("Playlist import cancelled by user");
                return anyhow::Ok(());
            };

            let span = tracing::debug_span!("import_playlist", playlist_id, path = %path.display());
            let ids: Vec<i64> = parse_m3u(File::open(path).await?)
                .map(|result| {
                    let pool = pool.clone();
                    async move {
                        let entry = match result {
                            Ok(entry) => entry,
                            Err(err) => {
                                error!(?err, "Error parsing M3U entry: {err}");
                                return Err(err.into());
                            }
                        };
                        let reference = match playlist_reference::decode(&entry.location) {
                            Ok(reference) => reference,
                            Err(err) => return Err(err),
                        };
                        if !reference.source().is_local() {
                            // No title/artist fallback: it could choose another account's copy.
                            let id = sqlx::query_scalar::<Sqlite, i64>("SELECT id FROM track WHERE source = $1 AND location = $2")
                                .bind(reference.source()).bind(reference.remote_id())
                                .fetch_optional(&pool).await?;
                            return id.map(Some).context("Remote playlist track is not indexed; original playlist was retained");
                        }
                        let location = Path::new(&entry.location);
                        let lookup_query = include_str!("../../queries/playlist/lookup_track.sql");
                        match sqlx::query_scalar::<Sqlite, i64>(lookup_query)
                            .bind(&entry.location)
                            .bind(entry.track_title)
                            .bind(entry.artist_name)
                            .bind(entry.album_title)
                            .bind(entry.track_artist_names)
                            .bind(entry.duration)
                            .bind(format!(
                                "%{}%",
                                location.file_stem()
                                    .and_then(OsStr::to_str)
                                    .unwrap_or_default()
                            ))
                            .fetch_one(&pool)
                            .await
                        {
                            Ok(id) => Ok(Some(id)),
                            Err(err) => {
                                warn!(
                                    ?err,
                                    "Failed to find track for '{}': {err}",
                                    location.display()
                                );
                                Ok(None)
                            }
                        }
                    }
                })
                .buffered(8)
                .try_filter_map(|id| std::future::ready(Ok(id)))
                .try_collect()
                .instrument(debug_span!(parent: &span, "lookup_tracks"))
                .await?;

            let mut tx = pool.begin().await?;

            let reset_query = include_str!("../../queries/playlist/empty_playlist.sql");
            sqlx::query(reset_query)
                .bind(playlist_id)
                .execute(&mut *tx)
                .await?;

            let insert_query = include_str!("../../queries/playlist/add_track.sql");
            for track_id in ids {
                sqlx::query(insert_query)
                    .bind(playlist_id)
                    .bind(track_id)
                    .execute(&mut *tx)
                    .instrument(debug_span!(parent: &span, "insert_track", track_id))
                    .await?;
            }

            tx.commit().await?;

            anyhow::Ok(())
        });

        if let Err(err) = task.err_into().await.flatten() {
            error!(?err, "Failed to import playlist: {err}");
            return;
        }

        playlist_tracker.update(cx, |_, cx| {
            cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
        })
    })
    .detach();
}
