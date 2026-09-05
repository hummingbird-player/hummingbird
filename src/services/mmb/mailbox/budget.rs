//! Conservative retained-payload charge. Shared metadata/reference storage is
//! charged per delivery, so sharing can only reduce actual retained memory.
use super::Event;
use crate::{
    playback::session::SessionEventKind,
    sources::{TrackLocation, TrackRef},
};

fn reference_bytes(reference: &TrackRef) -> usize {
    reference.source().as_str().len()
        + match reference.location() {
            TrackLocation::Local(path) => path.capacity(),
            TrackLocation::Remote(id) => id.capacity(),
        }
}
fn strings<'a>(values: impl IntoIterator<Item = &'a Option<String>>) -> usize {
    values.into_iter().flatten().map(String::capacity).sum()
}
pub(super) fn retained_bytes(event: &Event) -> usize {
    // Includes fixed event/permit/queue bookkeeping and progress-index storage.
    let fixed = 512;
    fixed
        + match event {
            Event::NewTrack(reference) => reference_bytes(reference),
            Event::Session(event) => match &event.kind {
                SessionEventKind::Started { reference, .. } => reference_bytes(reference),
                SessionEventKind::Metadata { metadata: m } => {
                    strings([
                        &m.title,
                        &m.artist,
                        &m.album,
                        &m.album_artist,
                        &m.album_mbid,
                        &m.isrc,
                    ]) + m.artists.capacity() * size_of::<String>()
                        + m.artists.iter().map(String::capacity).sum::<usize>()
                }
                SessionEventKind::Duration { .. }
                | SessionEventKind::State { .. }
                | SessionEventKind::Seek { .. }
                | SessionEventKind::Progress { .. }
                | SessionEventKind::Ended { .. } => 0,
            },
            Event::MetadataRecieved(m) => {
                size_of::<crate::media::metadata::Metadata>()
                    + strings([
                        &m.name,
                        &m.artist,
                        &m.album_artist,
                        &m.artist_sort,
                        &m.album_artist_sort,
                        &m.original_artist,
                        &m.composer,
                        &m.album,
                        &m.sort_album,
                        &m.grouping,
                        &m.disc_subtitle,
                        &m.label,
                        &m.catalog,
                        &m.isrc,
                        &m.mbid_album,
                        &m.lyrics,
                    ])
                    + [&m.artists, &m.album_artist_keys, &m.genres]
                        .into_iter()
                        .map(|values| {
                            values.capacity() * size_of::<String>()
                                + values.iter().map(String::capacity).sum::<usize>()
                        })
                        .sum::<usize>()
            }
            _ => 0,
        }
}
