use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use camino::Utf8PathBuf;
use gpui::{App, AppContext};

use crate::{
    library::{
        availability::{self, AvailabilitySnapshot, AvailabilityState},
        db::LibraryAccess,
        scan::ScanInterface,
        types::Track,
    },
    ui::models::Models,
};

pub fn snapshot<C: AppContext>(cx: &C) -> AvailabilitySnapshot {
    cx.read_global(|models: &Models, app| models.availability.read(app).snapshot())
}

pub fn is_track_path_available<C: AppContext>(cx: &C, path: &Path) -> bool {
    cx.read_global(|models: &Models, app| {
        models.availability.read(app).is_track_path_available(path)
    })
}

pub fn is_reference_available<C: AppContext>(cx: &C, reference: &crate::sources::TrackRef) -> bool {
    cx.read_global(|models: &Models, app| {
        models.availability.read(app).is_track_available(reference)
    })
}

pub fn is_track_available<C: AppContext>(cx: &C, track: &Track) -> bool {
    cx.read_global(|models: &Models, app| {
        models
            .availability
            .read(app)
            .is_indexed_track_available(&track.reference, track.present)
    })
}

pub fn has_available_tracks<C: AppContext>(cx: &C, tracks: &[Track]) -> bool {
    let availability = snapshot(cx);
    tracks
        .iter()
        .any(|track| availability.is_indexed_track_available(&track.reference, track.present))
}

pub fn album_has_available_tracks(cx: &mut App, album_id: i64) -> bool {
    let availability = snapshot(cx);
    cx.list_tracks_in_album(album_id)
        .map(|tracks| {
            tracks.iter().any(|track| {
                availability.is_indexed_track_available(&track.reference, track.present)
            })
        })
        .unwrap_or_default()
}

pub fn artist_has_available_tracks(cx: &mut App, artist_id: i64) -> bool {
    let availability = snapshot(cx);
    cx.get_all_tracks_by_artist(artist_id)
        .map(|tracks| {
            tracks.iter().any(|track| {
                availability.is_indexed_track_available(&track.reference, track.present)
            })
        })
        .unwrap_or_default()
}

pub fn start_monitor(
    cx: &mut App,
    availability_model: gpui::Entity<AvailabilityState>,
    roots: Vec<PathBuf>,
) {
    let mut events = availability::start_mount_monitor(roots);
    let scanner = cx.global::<ScanInterface>().clone();

    cx.spawn(async move |cx| {
        while events.recv().await.is_some() {
            // mount operations can emit multiple events, such as a network mount and its bind
            // mount; reconcile after a short burst rather than once per signal
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            while events.try_recv().is_ok() {}

            let roots = availability_model.read_with(cx, |state, _| state.configured_roots());
            let mounts = crate::RUNTIME
                .spawn_blocking(move || availability::current_mounts_for(&roots))
                .await
                .unwrap_or_default();

            let paths = availability_model.update(cx, |state, cx| {
                let (changed, reconnected) = state.reconcile_mounts(&mounts);
                if changed {
                    cx.notify();
                }
                reconnected
                    .into_iter()
                    .filter_map(|path| Utf8PathBuf::from_path_buf(path).ok())
                    .collect()
            });
            scanner.storage_available(paths).await;
        }
    })
    .detach();
}

pub fn update_roots(
    availability_model: &gpui::Entity<AvailabilityState>,
    roots: Vec<PathBuf>,
    cx: &mut App,
) {
    if availability_model.read(cx).configured_roots() == roots {
        return;
    }

    let mounts = availability::current_mounts_for(&roots);
    availability_model.update(cx, |state, cx| {
        if state.set_roots(roots, mounts) {
            cx.notify();
        }
    });
}
