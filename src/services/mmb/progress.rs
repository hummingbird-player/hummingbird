use crate::playback::thread::PlaybackState;

#[derive(Default)]
pub(super) struct ListenProgress {
    pub duration: u64,
    pub state: PlaybackState,
    pub submitted: bool,
    last_position: Option<u64>,
    accumulated: u64,
}

impl ListenProgress {
    pub fn reset(&mut self) {
        *self = Self {
            state: self.state,
            ..Self::default()
        };
    }

    pub fn position_changed(&mut self, position: u64) {
        // the first position is a baseline, including when enabled halfway through a track
        if let Some(previous) = self.last_position.replace(position)
            && self.state == PlaybackState::Playing
            && position.checked_sub(previous) == Some(1)
        {
            self.accumulated += 1;
        }
    }

    pub fn eligible(&self) -> bool {
        !self.submitted
            && self.duration >= 30
            && (self.accumulated > self.duration / 2 || self.accumulated > 240)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_position_and_seeks_do_not_count() {
        let mut progress = ListenProgress {
            state: PlaybackState::Playing,
            ..ListenProgress::default()
        };
        for position in [100, 101, 180, 40, 41, 41] {
            progress.position_changed(position);
        }
        assert_eq!(progress.accumulated, 2);
    }

    #[test]
    fn paused_positions_do_not_count() {
        let mut progress = ListenProgress::default();
        progress.position_changed(0);
        progress.state = PlaybackState::Playing;
        progress.position_changed(1);
        progress.state = PlaybackState::Paused;
        progress.position_changed(2);
        progress.position_changed(3);
        progress.state = PlaybackState::Playing;
        progress.position_changed(4);
        assert_eq!(progress.accumulated, 2);
    }

    #[test]
    fn buffering_positions_do_not_count() {
        let mut progress = ListenProgress {
            state: PlaybackState::Playing,
            ..ListenProgress::default()
        };
        progress.position_changed(0);
        progress.position_changed(1);
        progress.state = PlaybackState::Buffering;
        progress.position_changed(2);
        progress.position_changed(3);
        progress.state = PlaybackState::Playing;
        progress.position_changed(4);
        assert_eq!(progress.accumulated, 2);
    }

    #[test]
    fn qualification_keeps_the_existing_thresholds() {
        let mut progress = ListenProgress {
            duration: 30,
            accumulated: 15,
            ..ListenProgress::default()
        };
        assert!(!progress.eligible());
        progress.accumulated = 16;
        assert!(progress.eligible());
        progress.duration = 29;
        assert!(!progress.eligible());
        progress.duration = 1_000;
        progress.accumulated = 240;
        assert!(!progress.eligible());
        progress.accumulated = 241;
        assert!(progress.eligible());
        progress.submitted = true;
        assert!(!progress.eligible());
        progress.reset();
        assert!(!progress.submitted);
        assert_eq!(progress.accumulated, 0);
        assert_eq!(progress.duration, 0);
        assert_eq!(progress.last_position, None);
    }
}
