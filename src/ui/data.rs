use std::path::Path;

use crate::{
    media::{lookup_table::try_open_media, metadata::Metadata, traits::MediaProviderFeatures},
    playback::queue::{DataSource, QueueItemUIData},
};

#[tracing::instrument(level = "trace")]
pub(crate) fn read_metadata(path: &Path) -> anyhow::Result<QueueItemUIData> {
    let mut stream = try_open_media(path, MediaProviderFeatures::PROVIDES_METADATA)?
        .ok_or_else(|| anyhow::anyhow!("no metadata provider for {}", path.display()))?;
    stream.start_playback()?;

    let Metadata {
        name,
        artist,
        album_artist,
        ..
    } = stream.read_metadata()?;
    let ui_data = QueueItemUIData {
        name: name.as_ref().map(Into::into),
        artist_name: artist.as_ref().or(album_artist.as_ref()).map(Into::into),
        source: DataSource::Metadata,
        album_id: None,
        duration: stream.duration_ms().ok().map(|ms| ms as i64 / 1_000),
    };

    Ok(ui_data)
}
