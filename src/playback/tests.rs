//! Phase 0 test harness from the audio pipeline robustness plan
//! (`plans/audio-pipeline-robustness.html`).
//!
//! Several of these tests assert the *target* behavior rather than the
//! current one, and are expected to fail until later phases land:
//!
//! - unsigned round-trip / midpoint tests fail until C1/C2 (Phase 3)
//! - conversion-totality tests fail (panic) for I24/U24 until B4 (Phase 2)
//! - the allocation guard may fail until the Phase 1 hygiene items (A2, A3,
//!   A4, M1) land, at which point `WARM_UP_CYCLES` can drop to 0

mod alloc_guard;
mod bit_transparency;
mod eof_drain;
mod harness;
mod round_trip;
