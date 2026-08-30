pub mod track_item;

use std::sync::Arc;

use gpui::{AnyElement, App, Entity, IntoElement, SharedString};

use crate::{
    library::types::{DBString, Track},
    media::numbering::{NumberDisplayMode, format_track_position},
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

fn is_group_start(index: usize, prev: Option<&Track>, track: &Track) -> bool {
    index == 0
        || (track.track_number == Some(1) && track.track_section.is_none())
        || prev.is_some_and(|prev| prev.disc_number != track.disc_number)
}

impl TrackListing {
    pub fn new(
        cx: &mut App,
        tracks: Arc<Vec<Track>>,
        artist_name_visibility: ArtistNameVisibility,
        number_display_mode: NumberDisplayMode,
        show_go_to_album: bool,
        show_go_to_artist: bool,
    ) -> Self {
        let max_track_num_str = tracks
            .iter()
            .filter_map(|track| {
                format_track_position(
                    number_display_mode,
                    track.disc_number,
                    track.track_number,
                    track.track_section,
                )
            })
            .max_by_key(|label| label.chars().count())
            .map(SharedString::from);

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
                            index,
                            is_group_start(
                                index,
                                tracks_for_closure.get(index.wrapping_sub(1)),
                                track,
                            ),
                            artist_name_visibility.clone(),
                            TrackItemLeftField::TrackNum,
                            None,
                            number_display_mode,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::types::DBString;

    fn track(number: Option<i32>, section: Option<i32>, disc: Option<i32>) -> Track {
        Track {
            id: 0,
            title: DBString::default(),
            title_sortable: DBString::default(),
            album_id: None,
            track_number: number,
            track_section: section,
            disc_number: disc,
            duration: 0,
            created_at: chrono::DateTime::<chrono::Utc>::default(),
            genres: Vec::new(),
            tags: None,
            location: "".into(),
            artist_names: None,
            rg_track_gain: None,
            rg_track_peak: None,
            rg_album_gain: None,
            rg_album_peak: None,
            disc_subtitle: None,
            release_date: None,
            date_precision: None,
        }
    }

    #[test]
    fn finds_group_starts() {
        let one = track(Some(1), None, None);
        let one_one = track(Some(1), Some(1), None);
        let one_two = track(Some(1), Some(2), None);
        let two = track(Some(2), None, None);

        assert!(is_group_start(0, None, &one));
        assert!(!is_group_start(1, Some(&one), &one_one));
        assert!(!is_group_start(2, Some(&one_one), &one_two));
        assert!(!is_group_start(3, Some(&one_two), &two));
        assert!(is_group_start(
            3,
            Some(&track(Some(4), None, Some(1))),
            &track(Some(1), None, Some(2))
        ));
        assert!(is_group_start(
            2,
            Some(&track(Some(7), None, None)),
            &track(Some(1), None, None)
        ));
    }
}
