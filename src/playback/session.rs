//! The playback-to-MMBS state machine.
//!
//! A selected decoder is pending until its first frame is rendered. Rendering
//! activates the session; metadata, playback state, seeks, and cumulative
//! progress then update it until the final buffered frame produces `Ended`.
//! Multiple sessions can briefly be active while a gapless tail drains. Codec
//! lookahead and UI position notifications never contribute listening time.
use crate::{media::metadata::Metadata, sources::TrackRef};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(pub [u8; 16]);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Completed,
    Skipped,
    Stopped,
    Replaced,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub position_ms: u64,
    pub played_ms: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub album_mbid: Option<String>,
    pub track_number: Option<u64>,
    pub isrc: Option<String>,
}
impl From<&Metadata> for SessionMetadata {
    fn from(value: &Metadata) -> Self {
        Self {
            title: value.name.clone(),
            artist: value.artist.clone(),
            artists: value.artists.iter().cloned().collect(),
            album: value.album.clone(),
            album_artist: value.album_artist.clone(),
            album_mbid: value.mbid_album.clone(),
            track_number: value.track_current,
            isrc: value.isrc.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEvent {
    pub session: SessionId,
    pub sequence: u64,
    pub kind: SessionEventKind,
}
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEventKind {
    Started {
        reference: TrackRef,
        database_id: Option<i64>,
        started_at_ms: i64,
        position_ms: u64,
    },
    Metadata {
        metadata: SessionMetadata,
    },
    Duration {
        duration_ms: Option<u64>,
    },
    State {
        state: super::thread::PlaybackState,
        progress: Progress,
    },
    Seek {
        progress: Progress,
    },
    Progress {
        progress: Progress,
    },
    Ended {
        reason: EndReason,
        progress: Progress,
    },
}

struct Record {
    owner: u64,
    id: SessionId,
    reference: TrackRef,
    selected_at_ms: i64,
    started: bool,
    sequence: u64,
    metadata: SessionMetadata,
    duration_ms: Option<u64>,
    state: super::thread::PlaybackState,
    position_ns: u128,
    played_ns: u128,
    remainder: u128,
    remainder_rate: u32,
    last_progress_ms: u64,
    ending: Option<EndReason>,
}
impl Record {
    fn progress(&self) -> Progress {
        Progress {
            position_ms: (self.position_ns / 1_000_000).min(u64::MAX.into()) as u64,
            played_ms: (self.played_ns / 1_000_000).min(u64::MAX.into()) as u64,
        }
    }
    fn emit(&mut self, kind: SessionEventKind, output: &mut impl FnMut(SessionEvent)) {
        self.sequence += 1;
        output(SessionEvent {
            session: self.id,
            sequence: self.sequence,
            kind,
        });
    }
}
pub struct SessionTracker {
    records: VecDeque<Record>,
    current: Option<u64>,
    next_owner: u64,
}
impl Default for SessionTracker {
    fn default() -> Self {
        Self {
            records: VecDeque::with_capacity(64),
            current: None,
            next_owner: 1,
        }
    }
}
impl SessionTracker {
    pub fn select(&mut self, reference: TrackRef, now_ms: i64) -> u64 {
        let owner = self.next_owner;
        self.next_owner = self
            .next_owner
            .checked_add(1)
            .expect("playback owner space exhausted");
        self.current = Some(owner);
        self.records.push_back(Record {
            owner,
            id: SessionId(rand::random()),
            reference,
            selected_at_ms: now_ms,
            started: false,
            sequence: 0,
            metadata: Default::default(),
            duration_ms: None,
            state: super::thread::PlaybackState::Buffering,
            position_ns: 0,
            played_ns: 0,
            remainder: 0,
            remainder_rate: 0,
            last_progress_ms: 0,
            ending: None,
        });
        owner
    }
    pub fn end_current(&mut self, reason: EndReason) {
        if let Some(record) = self.current_record() {
            record.ending.get_or_insert(reason);
        }
        self.current = None;
    }
    fn current_record(&mut self) -> Option<&mut Record> {
        let owner = self.current?;
        self.records.iter_mut().find(|record| record.owner == owner)
    }
    pub fn metadata(&mut self, metadata: &Metadata, output: &mut impl FnMut(SessionEvent)) {
        if let Some(record) = self.current_record() {
            let metadata = SessionMetadata::from(metadata);
            if record.metadata == metadata {
                return;
            }
            record.metadata = metadata.clone();
            if record.started {
                record.emit(SessionEventKind::Metadata { metadata }, output);
            }
        }
    }
    pub fn duration(&mut self, duration: u64, output: &mut impl FnMut(SessionEvent)) {
        if let Some(record) = self.current_record() {
            let duration_ms = (duration > 0).then_some(duration);
            if record.duration_ms == duration_ms {
                return;
            }
            record.duration_ms = duration_ms;
            if record.started {
                record.emit(SessionEventKind::Duration { duration_ms }, output);
            }
        }
    }
    pub fn state(
        &mut self,
        state: super::thread::PlaybackState,
        output: &mut impl FnMut(SessionEvent),
    ) {
        if let Some(record) = self.current_record() {
            if record.state == state {
                return;
            }
            record.state = state;
            if record.started {
                record.emit(
                    SessionEventKind::State {
                        state,
                        progress: record.progress(),
                    },
                    output,
                );
            }
        }
    }
    pub fn seek(&mut self, position_ms: u64, output: &mut impl FnMut(SessionEvent)) {
        if let Some(record) = self.current_record() {
            record.position_ns = u128::from(position_ms) * 1_000_000;
            if record.started {
                record.emit(
                    SessionEventKind::Seek {
                        progress: record.progress(),
                    },
                    output,
                );
            }
        }
    }
    pub fn rendered(
        &mut self,
        owner: u64,
        frames: u64,
        rate: u32,
        now_ms: i64,
        output: &mut impl FnMut(SessionEvent),
    ) {
        if frames == 0 || rate == 0 {
            return;
        }
        let Some(record) = self.records.iter_mut().find(|record| record.owner == owner) else {
            return;
        };
        if !record.started {
            record.started = true;
            let elapsed_ms =
                (u128::from(frames) * 1000 / u128::from(rate)).min(i64::MAX as u128) as i64;
            record.emit(
                SessionEventKind::Started {
                    reference: record.reference.clone(),
                    database_id: None,
                    started_at_ms: now_ms.saturating_sub(elapsed_ms).max(record.selected_at_ms),
                    position_ms: record.progress().position_ms,
                },
                output,
            );
            record.emit(
                SessionEventKind::Metadata {
                    metadata: record.metadata.clone(),
                },
                output,
            );
            record.emit(
                SessionEventKind::Duration {
                    duration_ms: record.duration_ms,
                },
                output,
            );
            record.emit(
                SessionEventKind::State {
                    state: record.state,
                    progress: record.progress(),
                },
                output,
            );
        }
        if record.remainder_rate != 0 && record.remainder_rate != rate {
            record.remainder =
                record.remainder * u128::from(rate) / u128::from(record.remainder_rate);
        }
        record.remainder_rate = rate;
        let numerator = u128::from(frames) * 1_000_000_000 + record.remainder;
        let elapsed = numerator / u128::from(rate);
        record.remainder = numerator % u128::from(rate);
        record.played_ns = record.played_ns.saturating_add(elapsed);
        record.position_ns = record.position_ns.saturating_add(elapsed);
        let progress = record.progress();
        if progress.played_ms >= record.last_progress_ms.saturating_add(1000) {
            record.last_progress_ms = progress.played_ms;
            record.emit(SessionEventKind::Progress { progress }, output);
        }
    }
    /// An internal repeat crossed the actual output boundary. Retain the audio
    /// owner (including old queued tails), but start fresh service identity and
    /// listening totals. Metadata remains owned here, not in the marker queue.
    pub fn repeat_rendered(
        &mut self,
        owner: u64,
        position_ms: u64,
        now_ms: i64,
        output: &mut impl FnMut(SessionEvent),
    ) {
        let Some(record) = self.records.iter_mut().find(|record| record.owner == owner) else {
            return;
        };
        if record.started {
            record.emit(
                SessionEventKind::Ended {
                    reason: EndReason::Completed,
                    progress: record.progress(),
                },
                output,
            );
        }
        record.id = SessionId(rand::random());
        record.sequence = 0;
        record.started = false;
        record.selected_at_ms = now_ms;
        record.position_ns = u128::from(position_ms) * 1_000_000;
        record.played_ns = 0;
        record.remainder = 0;
        record.remainder_rate = 0;
        record.last_progress_ms = 0;
    }
    pub fn finish_ended(
        &mut self,
        mut pending: impl FnMut(u64) -> bool,
        output: &mut impl FnMut(SessionEvent),
    ) {
        let mut index = 0;
        while index < self.records.len() {
            let record = &self.records[index];
            if record.ending.is_none() || pending(record.owner) {
                index += 1;
                continue;
            }
            let mut record = self.records.remove(index).unwrap();
            if record.started {
                record.emit(
                    SessionEventKind::Ended {
                        reason: record.ending.unwrap(),
                        progress: record.progress(),
                    },
                    output,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
