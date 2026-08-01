use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use camino::Utf8PathBuf;
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer,
    notify::{
        RecommendedWatcher, RecursiveMode,
        event::{AccessKind, AccessMode, EventKind},
    },
};
use rustc_hash::FxHashSet;
use tokio::sync::mpsc::WeakSender;
use tracing::{error, info, warn};

use crate::{
    library::scan::ScanCommand,
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
    settings::scan::ScanSettings,
};

/// How long event bursts are coalesced before dispatching.
const DEBOUNCE_WINDOW: Duration = Duration::from_secs(2);

/// Batches touching more directories than this are promoted to a full scan, cheaper
/// than many targeted rescans.
const STORM_TARGET_CAP: usize = 200;

/// Watches library roots and forwards filesystem changes to the scanner task as rescan
/// commands - all real work happens there.
pub struct LibraryWatcher {
    // held for its Drop, which stops the debouncer thread
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    /// Roots covered by a live backend watch - a root leaves after being unmounted.
    watched: FxHashSet<Utf8PathBuf>,
    /// Roots that failed to watch or later went missing - re-checked periodically.
    unwatched: FxHashSet<Utf8PathBuf>,
    /// Unwatchable roots already logged about - retries stay quiet until one succeeds.
    warned: FxHashSet<Utf8PathBuf>,
}

impl LibraryWatcher {
    fn new(roots: &[Utf8PathBuf], cmd_tx: WeakSender<ScanCommand>) -> Option<Self> {
        let mut debouncer =
            match new_debouncer(DEBOUNCE_WINDOW, None, move |result: DebounceEventResult| {
                handle_debounced(result, &cmd_tx)
            }) {
                Ok(debouncer) => debouncer,
                Err(e) => {
                    error!("Could not start library watcher: {:?}", e);
                    return None;
                }
            };

        let mut watched = FxHashSet::default();
        let mut unwatched = FxHashSet::default();
        let mut warned = FxHashSet::default();
        let mut seen = FxHashSet::default();
        for root in roots {
            let Some(canonical) = root.canonicalize_utf8().ok() else {
                unwatched.insert(root.clone());
                continue;
            };

            if !seen.insert(canonical.clone()) {
                continue;
            }

            match debouncer.watch(canonical.as_std_path(), RecursiveMode::Recursive) {
                Ok(()) => {
                    watched.insert(canonical);
                }
                Err(e) => {
                    warn!("Could not watch {:?}: {:?}", canonical, e);
                    #[cfg(target_os = "linux")]
                    warn!("If this is a watch count limit, raise fs.inotify.max_user_watches");
                    warned.insert(canonical.clone());
                    unwatched.insert(canonical);
                }
            }
        }

        Some(Self {
            _debouncer: debouncer,
            watched,
            unwatched,
            warned,
        })
    }

    pub fn is_active(&self) -> bool {
        !self.watched.is_empty()
    }

    pub fn watched_roots(&self) -> Vec<Utf8PathBuf> {
        self.watched.iter().cloned().collect()
    }

    pub fn unwatched_roots(&self) -> Vec<Utf8PathBuf> {
        self.unwatched.iter().cloned().collect()
    }

    /// Apply probe results: drop roots that went missing, re-watch recoverable ones.
    /// Returns whether any watch was added.
    pub fn apply_probe(&mut self, lost: Vec<Utf8PathBuf>, recoverable: Vec<Utf8PathBuf>) -> bool {
        for root in lost {
            self.watched.remove(&root);
            self.unwatched.insert(root);
        }

        let mut recovered = false;
        for canonical in recoverable {
            if self.watched.contains(&canonical) {
                continue;
            }

            match self
                ._debouncer
                .watch(canonical.as_std_path(), RecursiveMode::Recursive)
            {
                Ok(()) => {
                    info!("Now watching {:?}", canonical);
                    self.warned.remove(&canonical);
                    self.watched.insert(canonical);
                    recovered = true;
                }
                Err(e) => {
                    if self.warned.insert(canonical.clone()) {
                        warn!("Could not watch {:?}: {:?}", canonical, e);
                    }
                    self.unwatched.insert(canonical);
                }
            }
        }

        recovered
    }
}

/// Which watched roots are gone and which unwatched ones can be watched again. Runs on
/// a blocking thread so a hung mount can't stall the scanner task.
pub(super) fn probe_roots(
    watched: &[Utf8PathBuf],
    unwatched: &[Utf8PathBuf],
) -> (Vec<Utf8PathBuf>, Vec<Utf8PathBuf>) {
    let lost: Vec<Utf8PathBuf> = watched
        .iter()
        .filter(|root| matches!(root.as_std_path().try_exists(), Ok(false)))
        .cloned()
        .collect();
    let recoverable: Vec<Utf8PathBuf> = unwatched
        .iter()
        .filter_map(|root| root.canonicalize_utf8().ok())
        .collect();
    (lost, recoverable)
}

/// Install the watcher for the current settings, or `None` if disabled or nothing is
/// configured. A watcher with no active roots is kept so it can retry them later.
pub fn arm(settings: &ScanSettings, cmd_tx: &WeakSender<ScanCommand>) -> Option<LibraryWatcher> {
    if !settings.watch_for_changes || settings.paths.is_empty() {
        return None;
    }
    LibraryWatcher::new(&settings.paths, cmd_tx.clone())
}

fn try_send_command(cmd_tx: &WeakSender<ScanCommand>, command: ScanCommand) -> bool {
    cmd_tx
        .upgrade()
        .is_some_and(|cmd_tx| cmd_tx.blocking_send(command).is_ok())
}

/// Blocking sends are deliberate - dropping a send would lose deletions.
fn handle_debounced(result: DebounceEventResult, cmd_tx: &WeakSender<ScanCommand>) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            // error batches may have lost events - settle with a full scan
            for e in &errors {
                warn!("Library watcher error: {:?}", e);
            }
            let _ = try_send_command(cmd_tx, ScanCommand::Scan);
            return;
        }
    };

    // drop access events, but keep close-write - backends may report it instead of modify
    let events: Vec<DebouncedEvent> = events
        .into_iter()
        .filter(|e| {
            e.need_rescan()
                || matches!(
                    e.event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Access(AccessKind::Close(AccessMode::Write))
                )
        })
        .collect();

    if events.is_empty() {
        return;
    }

    if events.iter().any(|e| e.need_rescan()) {
        let _ = try_send_command(cmd_tx, ScanCommand::Scan);
        return;
    }

    let paths: Vec<PathBuf> = events
        .iter()
        .flat_map(|e| e.event.paths.iter().cloned())
        .collect();
    let force_rescan = paths.iter().any(|path| is_album_art(path));
    let targets = affected_targets(&paths);
    if targets.is_empty() {
        return;
    }

    let command = if targets.len() > STORM_TARGET_CAP {
        ScanCommand::Scan
    } else {
        ScanCommand::RescanPaths {
            paths: targets.into_iter().collect(),
            respect_record: !force_rescan,
            // a moved-in tree produces no events for its contents - recurse to find them
            recursive: true,
        }
    };

    if !try_send_command(cmd_tx, command) {
        warn!("Could not queue watcher rescan, scanner is gone");
    }
}

/// Reduces event paths to rescan targets: media, art, and `.lrc` events map to their parent
/// directory, everything else to itself.
fn affected_targets(paths: &[PathBuf]) -> FxHashSet<Utf8PathBuf> {
    let mut targets = FxHashSet::default();
    let mut non_utf8: usize = 0;

    for path in paths {
        let Ok(path) = Utf8PathBuf::try_from(path.clone()) else {
            non_utf8 += 1;
            continue;
        };

        let is_media = can_be_read(
            path.as_std_path(),
            MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
        )
        .unwrap_or(false);
        let is_lyrics = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lrc"));
        let is_album_art = is_album_art(path.as_std_path());

        if is_media || is_lyrics || is_album_art {
            if let Some(parent) = path.parent() {
                targets.insert(parent.to_path_buf());
            }
            continue;
        }

        match path.as_std_path().try_exists() {
            Ok(true) => {
                if path.is_dir() {
                    targets.insert(path);
                }
            }
            Ok(false) => {
                targets.insert(path);
            }
            // unknown path that isn't media - leave it alone
            Err(_) => {}
        }
    }

    if non_utf8 > 0 {
        warn!("Dropped {} non-UTF-8 watcher event path(s)", non_utf8);
    }

    targets
}

fn is_album_art(path: &Path) -> bool {
    let stem = path.file_stem().and_then(|stem| stem.to_str());
    let extension = path.extension().and_then(|extension| extension.to_str());

    stem.is_some_and(|stem| {
        matches!(
            stem.to_ascii_lowercase().as_str(),
            "folder" | "cover" | "front"
        )
    }) && extension.is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use notify_debouncer_full::{
        DebouncedEvent,
        notify::{
            Event,
            event::{AccessKind, AccessMode, CreateKind, EventKind, Flag},
        },
    };
    use tokio::sync::mpsc::channel;

    use super::*;
    use crate::test_support::{TestDir, register_test_media_providers};

    fn handle_for_test(result: DebounceEventResult, tx: &tokio::sync::mpsc::Sender<ScanCommand>) {
        let weak_tx = tx.downgrade();
        handle_debounced(result, &weak_tx);
    }

    #[test]
    fn media_file_events_map_to_parent_directory() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();

        let targets = affected_targets(std::slice::from_ref(&file));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&dir.utf8_path().canonicalize_utf8().unwrap()));
    }

    #[test]
    fn lyrics_file_events_map_to_parent_directory() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("track.lrc");
        std::fs::write(&file, b"").unwrap();

        let targets = affected_targets(std::slice::from_ref(&file));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&dir.utf8_path().canonicalize_utf8().unwrap()));
    }

    #[test]
    fn album_art_file_events_map_to_parent_directory() {
        let dir = TestDir::new("watch-test");
        let file = dir.join("cover.JPG");

        let targets = affected_targets(std::slice::from_ref(&file));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&dir.utf8_path().canonicalize_utf8().unwrap()));
    }

    #[test]
    fn non_media_files_are_dropped() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("notes.txt");
        std::fs::write(&file, b"").unwrap();

        assert!(affected_targets(&[file]).is_empty());
    }

    #[test]
    fn deleted_media_file_maps_to_parent_directory() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");

        let targets = affected_targets(&[dir.join("gone.flac")]);
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&dir.utf8_path().canonicalize_utf8().unwrap()));
    }

    #[test]
    fn deleted_directory_maps_to_itself() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");

        let targets = affected_targets(&[dir.join("artist")]);
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&dir.utf8_join("artist")));
    }

    #[test]
    fn existing_directory_maps_to_itself() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let sub = dir.join("album");
        std::fs::create_dir(&sub).unwrap();

        let targets = affected_targets(std::slice::from_ref(&sub));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&Utf8PathBuf::try_from(sub).unwrap()));
    }

    #[test]
    fn access_events_are_dropped() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let (tx, mut rx) = channel(1);
        let event = DebouncedEvent::new(
            Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read))).add_path(file),
            Instant::now(),
        );

        // playback or editor open/close carries no content change - no rescan
        handle_for_test(Ok(vec![event]), &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn write_close_events_are_rescanned() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let (tx, mut rx) = channel(1);
        let event = DebouncedEvent::new(
            Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write))).add_path(file),
            Instant::now(),
        );

        handle_for_test(Ok(vec![event]), &tx);
        assert!(matches!(
            rx.blocking_recv(),
            Some(ScanCommand::RescanPaths { .. })
        ));
    }

    #[test]
    fn rescan_flag_promotes_to_full_scan() {
        let (tx, mut rx) = channel(1);
        let event = DebouncedEvent::new(
            Event::new(EventKind::Create(CreateKind::File)).set_flag(Flag::Rescan),
            Instant::now(),
        );

        handle_for_test(Ok(vec![event]), &tx);
        assert!(matches!(rx.blocking_recv(), Some(ScanCommand::Scan)));
    }

    #[test]
    fn storm_of_directories_promotes_to_full_scan() {
        register_test_media_providers();
        let (tx, mut rx) = channel(1);
        let events: Vec<DebouncedEvent> = (0..=STORM_TARGET_CAP)
            .map(|i| {
                DebouncedEvent::new(
                    Event::new(EventKind::Create(CreateKind::Folder))
                        .add_path(PathBuf::from(format!("/music/dir{i}"))),
                    Instant::now(),
                )
            })
            .collect();

        handle_for_test(Ok(events), &tx);
        assert!(matches!(rx.blocking_recv(), Some(ScanCommand::Scan)));
    }

    #[test]
    fn file_batch_queues_record_aware_rescan() {
        register_test_media_providers();
        let dir = TestDir::new("watch-test");
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let (tx, mut rx) = channel(1);
        let event = DebouncedEvent::new(
            Event::new(EventKind::Create(CreateKind::File)).add_path(file),
            Instant::now(),
        );

        handle_for_test(Ok(vec![event]), &tx);
        match rx.blocking_recv() {
            Some(ScanCommand::RescanPaths {
                paths,
                respect_record,
                recursive,
            }) => {
                assert!(respect_record);
                assert!(recursive);
                assert_eq!(paths.len(), 1);
            }
            other => panic!("expected a rescan command, got {:?}", other),
        }
    }

    #[test]
    fn album_art_batch_forces_rescan_of_recorded_files() {
        let dir = TestDir::new("watch-test");
        let file = dir.join("folder.png");
        let (tx, mut rx) = channel(1);
        let event = DebouncedEvent::new(
            Event::new(EventKind::Create(CreateKind::File)).add_path(file),
            Instant::now(),
        );

        handle_for_test(Ok(vec![event]), &tx);
        match rx.blocking_recv() {
            Some(ScanCommand::RescanPaths {
                respect_record,
                recursive,
                ..
            }) => {
                assert!(!respect_record);
                assert!(recursive);
            }
            other => panic!("expected an artwork rescan command, got {:?}", other),
        }
    }

    #[test]
    fn error_batch_promotes_to_full_scan() {
        let (tx, mut rx) = channel(1);
        handle_for_test(
            Err(vec![notify_debouncer_full::notify::Error::generic(
                "test error",
            )]),
            &tx,
        );
        assert!(matches!(rx.blocking_recv(), Some(ScanCommand::Scan)));
    }
}
