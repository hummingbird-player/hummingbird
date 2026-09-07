use std::sync::Arc;

use crate::{
    library::source::TrackRef,
    media::metadata::Metadata,
    playback::{events::PlaybackEvent, thread::PlaybackState},
    services::mmb::MediaEvent,
};

#[derive(Default)]
pub struct MediaProjection {
    track: Option<TrackRef>,
    metadata: Option<Arc<Metadata>>,
    duration: Option<u64>,
    position: Option<u64>,
    state: Option<PlaybackState>,
}

impl MediaProjection {
    pub fn project(&mut self, event: &PlaybackEvent) -> Option<MediaEvent> {
        match event {
            PlaybackEvent::SongChanged(path) => {
                let track = TrackRef::Local(path.clone());
                self.track = Some(track.clone());
                self.metadata = None;
                self.duration = None;
                self.position = None;
                self.state = None;
                Some(MediaEvent::TrackChanged(track))
            }
            PlaybackEvent::StateChanged(state) => {
                self.state = Some(*state);
                if *state == PlaybackState::Stopped {
                    self.track = None;
                    self.metadata = None;
                    self.duration = None;
                    self.position = None;
                }
                Some(MediaEvent::StateChanged(*state))
            }
            PlaybackEvent::MetadataUpdate(metadata) if self.track.is_some() => {
                let metadata = Arc::new(*metadata.clone());
                self.metadata = Some(metadata.clone());
                Some(MediaEvent::MetadataChanged(metadata))
            }
            PlaybackEvent::DurationChanged(ms) if self.track.is_some() => {
                let seconds = ms / 1_000;
                if self.duration.replace(seconds) == Some(seconds) {
                    return None;
                }
                Some(MediaEvent::DurationChanged(seconds))
            }
            PlaybackEvent::PositionChanged(ms) if self.track.is_some() => {
                let seconds = ms / 1_000;
                if self.position.replace(seconds) == Some(seconds) {
                    return None;
                }
                Some(MediaEvent::PositionChanged(seconds))
            }
            _ => None,
        }
    }

    pub fn bootstrap(&self) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        if let Some(track) = &self.track {
            events.push(MediaEvent::TrackChanged(track.clone()));
            if let Some(metadata) = &self.metadata {
                events.push(MediaEvent::MetadataChanged(metadata.clone()));
            }
            if let Some(duration) = self.duration {
                events.push(MediaEvent::DurationChanged(duration));
            }
            if let Some(position) = self.position {
                events.push(MediaEvent::PositionChanged(position));
            }
        }
        if let Some(state) = self.state {
            events.push(MediaEvent::StateChanged(state));
        } else if self.track.is_none() {
            events.push(MediaEvent::StateChanged(PlaybackState::Stopped));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_uses_the_current_track_and_known_fields_in_order() {
        let mut projection = MediaProjection::default();
        let metadata = Metadata {
            name: Some("song".into()),
            ..Metadata::default()
        };
        for event in [
            PlaybackEvent::SongChanged("song.flac".into()),
            PlaybackEvent::DurationChanged(200_800),
            PlaybackEvent::PositionChanged(70_900),
            PlaybackEvent::StateChanged(PlaybackState::Paused),
            PlaybackEvent::MetadataUpdate(Box::new(metadata.clone())),
        ] {
            projection.project(&event);
        }
        assert_eq!(
            projection.bootstrap(),
            vec![
                MediaEvent::TrackChanged(TrackRef::Local("song.flac".into())),
                MediaEvent::MetadataChanged(Arc::new(metadata)),
                MediaEvent::DurationChanged(200),
                MediaEvent::PositionChanged(70),
                MediaEvent::StateChanged(PlaybackState::Paused),
            ]
        );
    }

    #[test]
    fn progress_is_deduplicated_in_seconds_without_changing_seek_events() {
        let mut projection = MediaProjection::default();
        projection.project(&PlaybackEvent::SongChanged("song.flac".into()));
        let events: Vec<_> = [0, 100, 999, 1_000, 1_300, 40_000, 1_200]
            .into_iter()
            .filter_map(|ms| projection.project(&PlaybackEvent::PositionChanged(ms)))
            .collect();
        assert_eq!(
            events,
            vec![
                MediaEvent::PositionChanged(0),
                MediaEvent::PositionChanged(1),
                MediaEvent::PositionChanged(40),
                MediaEvent::PositionChanged(1),
            ]
        );
        assert_eq!(
            projection.project(&PlaybackEvent::DurationChanged(100_000)),
            Some(MediaEvent::DurationChanged(100))
        );
        assert_eq!(
            projection.project(&PlaybackEvent::DurationChanged(100_999)),
            None
        );
    }

    #[test]
    fn repeats_reset_known_fields_even_with_the_same_path() {
        let mut projection = MediaProjection::default();
        let song = PlaybackEvent::SongChanged("song.flac".into());
        let track = projection.project(&song).unwrap();
        projection.project(&PlaybackEvent::MetadataUpdate(Box::default()));
        projection.project(&PlaybackEvent::DurationChanged(100_000));
        projection.project(&PlaybackEvent::PositionChanged(50_000));
        projection.project(&PlaybackEvent::StateChanged(PlaybackState::Stopped));
        assert_eq!(projection.project(&song), Some(track.clone()));
        // don't replay the old stopped state while waiting for the new playing event
        assert_eq!(projection.bootstrap(), vec![track]);
        assert_eq!(
            projection.project(&PlaybackEvent::PositionChanged(50_000)),
            Some(MediaEvent::PositionChanged(50))
        );
    }

    #[test]
    fn stopped_clears_bootstrap_and_unrelated_events_are_ignored() {
        let mut projection = MediaProjection::default();
        projection.project(&PlaybackEvent::SongChanged("song.flac".into()));
        projection.project(&PlaybackEvent::StateChanged(PlaybackState::Stopped));
        for event in [
            PlaybackEvent::MetadataUpdate(Box::default()),
            PlaybackEvent::DurationChanged(100_000),
            PlaybackEvent::PositionChanged(100_000),
            PlaybackEvent::QueueUpdated,
            PlaybackEvent::QueuePositionChanged(2),
            PlaybackEvent::VolumeChanged(0.5),
            PlaybackEvent::AlbumArtUpdate(None),
            PlaybackEvent::SampleRateChanged(48_000),
        ] {
            assert_eq!(projection.project(&event), None);
        }
        assert_eq!(
            projection.bootstrap(),
            vec![MediaEvent::StateChanged(PlaybackState::Stopped)]
        );
    }
}
