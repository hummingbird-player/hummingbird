//! Pure session reduction shared by direct scrobblers. Inputs and work items are
//! owned values; neither service requests nor UI cadence participate in counting.
use crate::{
    playback::{
        session::{Progress, SessionEvent, SessionEventKind, SessionId, SessionMetadata},
        thread::PlaybackState,
    },
    sources::TrackRef,
};
use smallvec::SmallVec;
use std::collections::VecDeque;

const MAX_SESSIONS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Listen {
    pub session: SessionId,
    pub reference: TrackRef,
    pub started_at_ms: i64,
    pub metadata: SessionMetadata,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Work {
    /// None invalidates an unsent display update on replacement/pause/end.
    NowPlaying(Option<Listen>),
    Submit(Listen),
}

struct Record {
    listen: Listen,
    sequence: u64,
    played_ms: u64,
    state: PlaybackState,
    submitted: bool,
    displayed: Option<(SessionMetadata, Option<u64>)>,
}

/// Policy intent shared with source reporting: at least 30 seconds in length,
/// and strictly more than half the track or four minutes actually rendered.
pub fn eligible(duration_ms: Option<u64>, played_ms: u64) -> bool {
    duration_ms.is_some_and(|duration| {
        duration >= 30_000 && (played_ms > duration / 2 || played_ms > 240_000)
    })
}

pub struct ScrobbleReducer {
    enabled: bool,
    records: VecDeque<Record>,
    finished: VecDeque<SessionId>,
    current: Option<SessionId>,
}

impl ScrobbleReducer {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            records: VecDeque::with_capacity(MAX_SESSIONS),
            finished: VecDeque::with_capacity(MAX_SESSIONS),
            current: None,
        }
    }

    pub fn contains(&self, session: SessionId) -> bool {
        self.records
            .iter()
            .any(|record| record.listen.session == session)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.records.clear();
            self.finished.clear();
            self.current = None;
        }
        self.enabled = enabled;
    }

    pub fn event(&mut self, event: SessionEvent) -> SmallVec<[Work; 2]> {
        let mut work = SmallVec::new();
        if !self.enabled {
            return work;
        }
        if let SessionEventKind::Started {
            reference,
            started_at_ms,
            ..
        } = event.kind
        {
            if event.sequence != 1
                || self
                    .records
                    .iter()
                    .any(|r| r.listen.session == event.session)
                || self.finished.contains(&event.session)
            {
                return work;
            }
            // The playback producer permits at most 64 simultaneous owners.
            // Reject malformed excess starts rather than evicting an active
            // listen or allocating without a bound.
            if self.records.len() == MAX_SESSIONS {
                tracing::warn!("Scrobble session limit exceeded");
                return work;
            }
            self.current = Some(event.session);
            self.records.push_back(Record {
                listen: Listen {
                    session: event.session,
                    reference,
                    started_at_ms,
                    metadata: Default::default(),
                    duration_ms: None,
                },
                sequence: event.sequence,
                played_ms: 0,
                state: PlaybackState::Playing,
                submitted: false,
                displayed: None,
            });
            work.push(Work::NowPlaying(None));
            return work;
        }
        let Some(index) = self
            .records
            .iter()
            .position(|r| r.listen.session == event.session)
        else {
            // Never infer a missing start from metadata/progress after enabling.
            return work;
        };
        let record = &mut self.records[index];
        if event.sequence <= record.sequence {
            return work;
        }
        record.sequence = event.sequence;
        let mut ended = false;
        let progress: Option<Progress> = match event.kind {
            SessionEventKind::Metadata { metadata } => {
                record.listen.metadata = metadata;
                None
            }
            SessionEventKind::Duration { duration_ms } => {
                record.listen.duration_ms = duration_ms.filter(|v| *v > 0);
                None
            }
            SessionEventKind::State { state, progress } => {
                record.state = state;
                Some(progress)
            }
            SessionEventKind::Progress { progress } | SessionEventKind::Seek { progress } => {
                Some(progress)
            }
            SessionEventKind::Ended { progress, .. } => {
                ended = true;
                Some(progress)
            }
            SessionEventKind::Started { .. } => unreachable!(),
        };
        if let Some(progress) = progress {
            // Cumulative totals permit coalescing and seek jumps without adding
            // wall time, position differences, or duplicate deliveries.
            record.played_ms = record.played_ms.max(progress.played_ms);
        }
        let metadata = &record.listen.metadata;
        let has_metadata = metadata
            .artist
            .as_ref()
            .is_some_and(|v| !v.trim().is_empty())
            && metadata
                .title
                .as_ref()
                .is_some_and(|v| !v.trim().is_empty());
        if self.current == Some(event.session) {
            if ended || record.state != PlaybackState::Playing || !has_metadata {
                if record.displayed.take().is_some() {
                    work.push(Work::NowPlaying(None));
                }
            } else if record.displayed.as_ref().is_none_or(|(old, duration)| {
                old != metadata || *duration != record.listen.duration_ms
            }) {
                record.displayed = Some((metadata.clone(), record.listen.duration_ms));
                work.push(Work::NowPlaying(Some(record.listen.clone())));
            }
        }
        if !record.submitted
            && has_metadata
            && eligible(record.listen.duration_ms, record.played_ms)
        {
            record.submitted = true;
            work.push(Work::Submit(record.listen.clone()));
        }
        if ended {
            self.records.remove(index);
            if self.current == Some(event.session) {
                self.current = None;
            }
            if self.finished.len() == MAX_SESSIONS {
                self.finished.pop_front();
            }
            self.finished.push_back(event.session);
        }
        work
    }
}

#[cfg(test)]
mod tests;
