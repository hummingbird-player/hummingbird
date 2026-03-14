pub mod track_item;

use std::sync::Arc;

use gpui::{App, Entity, IntoElement, ListAlignment, ListState, Pixels, Window};

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
    track_list_state: ListState,
}

impl TrackListing {
    pub fn new(
        cx: &mut App,
        tracks: Arc<Vec<Track>>,
        overdraw: Pixels,
        artist_name_visibility: ArtistNameVisibility,
        vinyl_numbering: bool,
        show_go_to_album: bool,
        show_go_to_artist: bool,
    ) -> Self {
        let state = ListState::new(tracks.len(), ListAlignment::Top, overdraw);

        // find biggest track number and provide it to track item for measurement
        let max_track_num_str = tracks
            .iter()
            .filter_map(|t| t.track_number)
            .max()
            .map(|n| format!("{}", n).into());

        Self {
            tracks: Arc::new(
                tracks
                    .iter()
                    .enumerate()
                    .map(move |(index, track)| {
                        TrackItem::new(
                            cx,
                            track.clone(),
                            index == 0 || track.track_number == Some(1),
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
                    .collect(),
            ),
            original_tracks: tracks,
            track_list_state: state,
        }
    }

    pub fn tracks(&self) -> &Arc<Vec<Track>> {
        &self.original_tracks
    }

    pub fn track_list_state(&self) -> &ListState {
        &self.track_list_state
    }

    pub fn make_render_fn(
        &self,
    ) -> impl Fn(usize, &mut Window, &mut App) -> gpui::AnyElement + Clone + 'static {
        let tracks = self.tracks.clone();
        move |idx, _, _| tracks[idx].clone().into_any_element()
    }
}
