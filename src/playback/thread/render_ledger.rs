//! Attribute rendered frames to the owner of their submission, including audio
//! still queued across decoder changes. This is playback-control state; the
//! realtime callback only advances its device counter.
use crate::devices::render_clock::RenderSnapshot;
use smallvec::SmallVec;
use std::collections::VecDeque;

const MAX_SEGMENTS: usize = 64;
// A one-sample codec loop can place many boundaries inside one DSP block. Keep
// these small markers separate from session metadata and bounded on the host.
const MAX_REPEATS: usize = 16_384;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderedFrames {
    pub owner: u64,
    pub frames: u64,
    pub sample_rate: u32,
    pub repeat_before: Option<u64>,
}
struct Repeat {
    owner: u64,
    at: u64,
    position_ms: u64,
}
struct Segment {
    owner: Option<u64>,
    frames: u64,
}
pub struct RenderLedger {
    queued: VecDeque<Segment>,
    rendered: SmallVec<[RenderedFrames; 4]>,
    observed: u64,
    repeats: VecDeque<Repeat>,
    rendered_repeats: usize,
    // The unsubmitted suffix of queued ownership, reserved for DSP lookahead.
    reserved: u64,
}
impl Default for RenderLedger {
    fn default() -> Self {
        Self {
            queued: VecDeque::with_capacity(MAX_SEGMENTS),
            rendered: SmallVec::new(),
            observed: 0,
            repeats: VecDeque::new(),
            rendered_repeats: 0,
            reserved: 0,
        }
    }
}
impl RenderLedger {
    fn accepted_end(&self) -> u64 {
        self.queued
            .iter()
            .fold(self.observed, |end, segment| {
                end.saturating_add(segment.frames)
            })
            .saturating_sub(self.reserved)
    }
    /// Preserve ownership of the resampler tail before priming another track.
    /// Existing reservations describe older tracks in the same continuous DSP
    /// stream; append only this track's additional suffix.
    pub fn reserve_tail(&mut self, owner: Option<u64>, frames: u64) -> bool {
        if frames < self.reserved {
            self.discard_reserved_frames(self.reserved - frames);
        }
        let additional = frames.saturating_sub(self.reserved);
        if additional == 0 {
            return true;
        }
        if self.queued.back().is_none_or(|last| last.owner != owner) && !self.can_submit() {
            return false;
        }
        self.append(owner, additional);
        self.reserved = frames;
        true
    }
    fn discard_reserved_frames(&mut self, mut frames: u64) {
        self.reserved -= frames;
        while frames > 0 {
            let last = self.queued.back_mut().expect("reserved ownership exists");
            let removed = frames.min(last.frames);
            last.frames -= removed;
            frames -= removed;
            if last.frames == 0 {
                self.queued.pop_back();
            }
        }
    }
    pub fn discard_reserved(&mut self) {
        self.discard_reserved_frames(self.reserved);
        self.discard_unsubmitted_repeats();
    }
    /// Mark a boundary after all accepted output plus an unsubmitted DSP tail.
    /// No session event is produced until a post-boundary frame is rendered.
    pub fn repeat_after(&mut self, owner: u64, pending_frames: u64, position_ms: u64) -> bool {
        if self.repeats.len() + self.rendered_repeats >= MAX_REPEATS {
            return false;
        }
        let at = self.accepted_end().saturating_add(pending_frames);
        if self.repeats.back().is_some_and(|last| last.at > at) {
            return false;
        }
        self.repeats.push_back(Repeat {
            owner,
            at,
            position_ms,
        });
        true
    }
    /// Changing decoders discards unsubmitted audio. Already queued repeats still
    /// belong to the old track and must survive until that audio is rendered.
    pub fn discard_unsubmitted_repeats(&mut self) {
        let retained_end = self.accepted_end().saturating_add(self.reserved);
        self.repeats.retain(|marker| marker.at < retained_end);
    }
    /// Backpressure at the producer, never in the realtime callback. The host
    /// drains rendered updates before accepting further distinct owners.
    pub fn can_submit(&self) -> bool {
        // Reserve a slot for a partially rendered front segment's result.
        self.rendered.len() + self.queued.len() < MAX_SEGMENTS - 1
    }
    pub fn submitted(&mut self, owner: Option<u64>, frames: u64) {
        let carried = frames.min(self.reserved);
        self.reserved -= carried;
        self.append(owner, frames - carried);
    }
    fn append(&mut self, owner: Option<u64>, frames: u64) {
        if frames == 0 {
            return;
        }
        if let Some(last) = self.queued.back_mut().filter(|last| last.owner == owner) {
            last.frames = last.frames.saturating_add(frames);
        } else {
            debug_assert!(self.queued.len() < MAX_SEGMENTS);
            self.queued.push_back(Segment { owner, frames });
        }
    }
    pub fn poll(&mut self, snapshot: RenderSnapshot) {
        let mut remaining = snapshot.frames.saturating_sub(self.observed);
        if self.reserved > 0 {
            remaining = remaining.min(self.accepted_end().saturating_sub(self.observed));
        }
        while remaining > 0 {
            let mut repeat_before = None;
            while self
                .repeats
                .front()
                .is_some_and(|marker| marker.at <= self.observed)
            {
                let marker = self.repeats.pop_front().unwrap();
                if self
                    .queued
                    .front()
                    .is_some_and(|segment| segment.owner == Some(marker.owner))
                {
                    repeat_before = Some(marker.position_ms);
                }
            }
            let Some(front) = self.queued.front_mut() else {
                break;
            };
            let count = remaining.min(front.frames).min(
                self.repeats
                    .front()
                    .map_or(u64::MAX, |marker| marker.at.saturating_sub(self.observed)),
            );
            if let Some(owner) = front.owner {
                if let Some(existing) = self.rendered.last_mut().filter(|v| {
                    repeat_before.is_none()
                        && v.owner == owner
                        && v.sample_rate == snapshot.sample_rate
                }) {
                    existing.frames = existing.frames.saturating_add(count);
                } else {
                    // At most one entry per retained marker plus the bounded
                    // track segments. Drain the full callback prefix on reset.
                    debug_assert!(self.rendered.len() < MAX_REPEATS + MAX_SEGMENTS * 2);
                    self.rendered_repeats += usize::from(repeat_before.is_some());
                    self.rendered.push(RenderedFrames {
                        owner,
                        frames: count,
                        sample_rate: snapshot.sample_rate,
                        repeat_before,
                    });
                }
            }
            front.frames -= count;
            remaining -= count;
            self.observed = self.observed.saturating_add(count);
            if front.frames == 0 {
                self.queued.pop_front();
            }
        }
    }
    /// Call only after the output has finished resetting/closing. Polling first
    /// preserves its last rendered prefix, then discarded queue entries vanish.
    pub fn reset(&mut self, snapshot: RenderSnapshot) {
        self.poll(snapshot);
        self.queued.clear();
        self.repeats.clear();
        self.reserved = 0;
        self.observed = snapshot.frames;
    }
    pub fn new_stream(&mut self) {
        self.queued.clear();
        self.repeats.clear();
        self.reserved = 0;
        self.observed = 0;
    }
    pub fn take_rendered(&mut self) -> SmallVec<[RenderedFrames; 4]> {
        self.rendered_repeats = 0;
        std::mem::take(&mut self.rendered)
    }
    pub fn has_pending(&self, owner: u64) -> bool {
        self.queued.iter().any(|v| v.owner == Some(owner))
    }
}

#[cfg(test)]
mod tests;
