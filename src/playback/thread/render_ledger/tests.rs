use super::*;
fn at(frames: u64) -> RenderSnapshot {
    RenderSnapshot {
        frames,
        submitted_frames: frames,
        sample_rate: 48000,
    }
}
#[test]
fn gapless_rendering_retains_the_previous_owner_until_its_tail_is_consumed() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 48000);
    ledger.submitted(Some(2), 48000);
    ledger.poll(at(60000));
    assert_eq!(
        ledger.take_rendered().as_slice(),
        &[
            RenderedFrames {
                owner: 1,
                frames: 48000,
                sample_rate: 48000,
                repeat_before: None,
            },
            RenderedFrames {
                owner: 2,
                frames: 12000,
                sample_rate: 48000,
                repeat_before: None,
            },
        ]
    );
    assert!(!ledger.has_pending(1));
    assert!(ledger.has_pending(2));
    ledger.poll(at(96000));
    assert_eq!(ledger.take_rendered()[0].frames, 36000);
}
#[test]
fn reset_counts_only_rendered_prefix_and_reopen_preserves_undelivered_updates() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 100);
    ledger.reset(at(30));
    assert!(!ledger.has_pending(1));
    ledger.submitted(Some(2), 100);
    ledger.poll(at(50));
    assert_eq!(
        ledger.take_rendered().as_slice(),
        &[
            RenderedFrames {
                owner: 1,
                frames: 30,
                sample_rate: 48000,
                repeat_before: None,
            },
            RenderedFrames {
                owner: 2,
                frames: 20,
                sample_rate: 48000,
                repeat_before: None,
            },
        ]
    );
    ledger.reset(at(60));
    ledger.new_stream();
    ledger.submitted(Some(2), 40);
    ledger.poll(RenderSnapshot {
        frames: 40,
        submitted_frames: 40,
        sample_rate: 96000,
    });
    assert_eq!(
        ledger.take_rendered().as_slice(),
        &[
            RenderedFrames {
                owner: 2,
                frames: 10,
                sample_rate: 48000,
                repeat_before: None,
            },
            RenderedFrames {
                owner: 2,
                frames: 40,
                sample_rate: 96000,
                repeat_before: None,
            },
        ]
    );
}
#[test]
fn queued_audio_and_underruns_do_not_increase_rendered_totals() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 48000);
    ledger.poll(at(0));
    assert!(ledger.take_rendered().is_empty());
    ledger.poll(at(24000));
    assert_eq!(ledger.take_rendered()[0].frames, 24000);
    for _ in 0..20 {
        ledger.poll(at(24000));
    }
    assert!(ledger.take_rendered().is_empty());
}
#[test]
fn attribution_applies_backpressure_without_losing_rendered_transitions() {
    let mut ledger = RenderLedger::default();
    let mut accepted = 0;
    while ledger.can_submit() {
        ledger.submitted(Some(accepted), 2);
        accepted += 1;
    }
    assert!(accepted < MAX_SEGMENTS as u64);
    ledger.poll(at(accepted * 2 - 1));
    assert!(!ledger.can_submit());
    ledger.reset(at(accepted * 2));
    let events = ledger.take_rendered();
    assert_eq!(events.len(), accepted as usize);
    assert!(events.iter().all(|v| v.frames == 2));
    assert!(ledger.can_submit());
}
#[test]
fn steady_state_attribution_does_not_allocate() {
    let mut ledger = RenderLedger::default();
    let (_, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
        for index in 1..=1000 {
            ledger.submitted(Some(1), 1024);
            ledger.poll(at(index * 1024));
            assert_eq!(ledger.take_rendered()[0].frames, 1024);
        }
    });
    assert_eq!(allocations, 0);
}

#[test]
fn repeats_split_only_rendered_frames_and_survive_a_partial_reset() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 100);
    assert!(ledger.repeat_after(1, 20, 500));
    ledger.submitted(Some(1), 80);
    assert!(ledger.repeat_after(1, 0, 500));
    ledger.submitted(Some(1), 40);
    ledger.poll(at(120));
    let prefix = ledger.take_rendered();
    assert_eq!(prefix[0].frames, 120);
    assert_eq!(prefix[0].repeat_before, None);
    ledger.poll(at(120));
    assert!(ledger.take_rendered().is_empty());
    ledger.reset(at(190));
    let rendered = ledger.take_rendered();
    assert_eq!(
        rendered.iter().map(|v| v.frames).collect::<Vec<_>>(),
        [60, 10]
    );
    assert!(rendered.iter().all(|v| v.repeat_before == Some(500)));
    assert!(!ledger.has_pending(1));
    ledger.submitted(Some(2), 30);
    ledger.poll(at(220));
    assert!(
        ledger
            .take_rendered()
            .iter()
            .all(|v| v.repeat_before.is_none())
    );
}

#[test]
fn replacing_a_decoder_retains_queued_repeats_and_discards_unsubmitted_ones() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 100);
    assert!(ledger.repeat_after(1, 0, 10));
    ledger.submitted(Some(1), 20);
    assert!(ledger.repeat_after(1, 50, 20));
    ledger.discard_unsubmitted_repeats();
    ledger.submitted(Some(2), 100);
    ledger.poll(at(220));
    let rendered = ledger.take_rendered();
    assert_eq!(rendered.len(), 3);
    assert_eq!(
        (
            rendered[1].owner,
            rendered[1].frames,
            rendered[1].repeat_before
        ),
        (1, 20, Some(10))
    );
    assert_eq!(
        (
            rendered[2].owner,
            rendered[2].frames,
            rendered[2].repeat_before
        ),
        (2, 100, None)
    );
}

#[test]
fn a_repeat_burst_is_bounded_and_reset_retains_every_audible_boundary() {
    let mut ledger = RenderLedger::default();
    for frame in 0..MAX_REPEATS {
        assert!(ledger.repeat_after(1, frame as u64, 0));
    }
    assert!(!ledger.repeat_after(1, MAX_REPEATS as u64, 0));
    ledger.submitted(Some(1), MAX_REPEATS as u64);
    ledger.reset(at(MAX_REPEATS as u64));
    let rendered = ledger.take_rendered();
    assert_eq!(rendered.len(), MAX_REPEATS);
    assert!(
        rendered
            .iter()
            .all(|v| v.frames == 1 && v.repeat_before == Some(0))
    );
    assert!(ledger.repeat_after(1, 0, 0));
}

#[test]
fn several_unsubmitted_dsp_tails_keep_their_owners_across_partial_writes() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 100);
    ledger.poll(at(100));
    ledger.take_rendered();
    assert!(ledger.reserve_tail(Some(1), 20));
    assert!(ledger.has_pending(1));
    ledger.submitted(Some(2), 5); // First five frames still belong to track 1.
    assert!(ledger.reserve_tail(Some(2), 25)); // Remaining 15 old + 10 new.
    ledger.submitted(Some(3), 30);
    ledger.poll(at(135));
    let rendered = ledger.take_rendered();
    assert_eq!(
        rendered
            .iter()
            .map(|v| (v.owner, v.frames))
            .collect::<Vec<_>>(),
        [(1, 20), (2, 10), (3, 5)]
    );
    assert!(!ledger.has_pending(1));
    assert!(!ledger.has_pending(2));
}

#[test]
fn discarding_dsp_keeps_only_the_accepted_prefix_and_its_repeat_markers() {
    let mut ledger = RenderLedger::default();
    ledger.submitted(Some(1), 100);
    assert!(ledger.repeat_after(1, 10, 50));
    assert!(ledger.reserve_tail(Some(1), 20));
    ledger.discard_unsubmitted_repeats(); // A preserved tail still contains this marker.
    ledger.submitted(Some(2), 15);
    ledger.discard_reserved();
    ledger.submitted(Some(2), 10);
    ledger.reset(at(125));
    let rendered = ledger.take_rendered();
    assert_eq!(
        rendered
            .iter()
            .map(|v| (v.owner, v.frames, v.repeat_before))
            .collect::<Vec<_>>(),
        [(1, 110, None), (1, 5, Some(50)), (2, 10, None)]
    );
}

#[test]
fn tail_owner_capacity_does_not_discard_accepted_audio() {
    let mut ledger = RenderLedger::default();
    let mut owners = 0;
    while ledger.reserve_tail(Some(owners), owners + 1) {
        owners += 1;
    }
    assert!(owners < MAX_SEGMENTS as u64);
    ledger.submitted(Some(999), owners);
    ledger.poll(at(owners));
    let rendered = ledger.take_rendered();
    assert_eq!(rendered.len(), owners as usize);
    assert!(
        rendered
            .iter()
            .enumerate()
            .all(|(index, v)| v.owner == index as u64 && v.frames == 1)
    );
    assert!(ledger.reserve_tail(Some(999), 1));
}
