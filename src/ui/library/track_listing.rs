pub mod track_item;

use std::sync::Arc;

use gpui::{AnyElement, App, Entity, IntoElement};

use crate::{
    library::types::{DBString, Track},
    ui::library::track_listing::track_item::TrackItemLeftField,
};
use track_item::TrackItem;

#[derive(Clone, Debug, PartialEq)]
pub enum ArtistNameVisibility {
    Always,
    Never,
    OnlyIfDifferent(Option<DBString>),
}

#[derive(Clone)]
pub struct TrackListing {
    // TODO: replace this with Arc<Vec<i64>>, memoize TrackItem, fetch on load instead of before
    tracks: Arc<Vec<Entity<TrackItem>>>,
    original_tracks: Arc<Vec<Track>>,
}

impl TrackListing {
    pub fn new(
        cx: &mut App,
        tracks: Arc<Vec<Track>>,
        artist_name_visibility: ArtistNameVisibility,
        vinyl_numbering: bool,
        show_go_to_album: bool,
        show_go_to_artist: bool,
    ) -> Self {
        // find biggest track number and provide it to track item for measurement
        let max_track_num_str = tracks
            .iter()
            .filter_map(|t| t.track_number)
            .max()
            .map(|n| format!("{}", n).into());

        Self {
            tracks: Arc::new({
                let tracks_for_closure = tracks.clone();
                tracks
                    .iter()
                    .enumerate()
                    .map(move |(index, track)| {
                        TrackItem::new(
                            cx,
                            track.clone(),
                            index == 0
                                || track.track_number == Some(1)
                                || tracks_for_closure
                                    .get(index - 1)
                                    .is_some_and(|t| t.disc_number != track.disc_number),
                            artist_name_visibility.clone(),
                            TrackItemLeftField::TrackNum,
                            None,
                            vinyl_numbering,
                            max_track_num_str.clone(),
                            None,
                            show_go_to_album,
                            show_go_to_artist,
                        )
                    })
                    .collect()
            }),
            original_tracks: tracks,
        }
    }

    pub fn tracks(&self) -> &Arc<Vec<Track>> {
        &self.original_tracks
    }

    pub fn track_elements(&self) -> Vec<AnyElement> {
        self.tracks
            .iter()
            .cloned()
            .map(|track| track.into_any_element())
            .collect()
    }
}
