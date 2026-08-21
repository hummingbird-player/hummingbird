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
    library::scan::control::ScanCommand,
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
    settings::scan::ScanSettings,
};

const DEBOUNCE_WINDOW: Duration = Duration::from_secs(2);
/// If one event batch touches more dirs than this, do a full scan instead.
const STORM_TARGET_CAP: usize = 200;

pub(super) struct LibraryWatcher {
    // kept so Drop stops the notify thread
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    watched: FxHashSet<Utf8PathBuf>,
    unwatched: FxHashSet<Utf8PathBuf>,
    /// Roots we've already warned about, so retries don't spam the log.
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

    pub(super) fn is_active(&self) -> bool {
        !self.watched.is_empty()
    }

    pub(super) fn watched_roots(&self) -> Vec<Utf8PathBuf> {
        self.watched.iter().cloned().collect()
    }

    pub(super) fn unwatched_roots(&self) -> Vec<Utf8PathBuf> {
        self.unwatched.iter().cloned().collect()
    }

    /// After a probe: stop watching lost roots, try to watch recovered ones. True if anything new is watched.
    pub(super) fn apply_probe(
        &mut self,
        lost: Vec<Utf8PathBuf>,
        recoverable: Vec<Utf8PathBuf>,
    ) -> bool {
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

/// Check root availability on a blocking thread so dead network mounts don't stall the scanner.
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

pub(super) fn arm(
    settings: &ScanSettings,
    cmd_tx: &WeakSender<ScanCommand>,
) -> Option<LibraryWatcher> {
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

fn handle_debounced(result: DebounceEventResult, cmd_tx: &WeakSender<ScanCommand>) {
    let events = match result {
        Ok(events) => events,
        Err(errors) => {
            // error batches may have lost events - full scan to catch up
            for e in &errors {
                warn!("Library watcher error: {:?}", e);
            }
            let _ = try_send_command(cmd_tx, ScanCommand::Scan);
            return;
        }
    };

    // ignore plain access events, but keep close-write - some backends use it instead of modify
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
            // a moved-in folder doesn't emit events for its contents - recurse to find them
            recursive: true,
        }
    };

    if !try_send_command(cmd_tx, command) {
        warn!("Could not queue watcher rescan, scanner is gone");
    }
}

/// Map event paths to rescan targets: media/art/lyrics use the parent folder, directories use themselves.
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
    fn media_lyrics_and_art_events_map_to_parent_directory() {
        register_test_media_providers();
        for (name, contents) in [
            ("track.flac", Some(b"".as_slice())),
            ("track.lrc", Some(b"".as_slice())),
            ("cover.JPG", None),
            ("gone.flac", None),
        ] {
            let dir = TestDir::new("watch-test");
            let file = dir.join(name);
            if let Some(contents) = contents {
                std::fs::write(&file, contents).unwrap();
            }

            let targets = affected_targets(std::slice::from_ref(&file));
            let mut expected = FxHashSet::default();
            expected.insert(dir.utf8_path().canonicalize_utf8().unwrap());
            assert_eq!(targets, expected);
        }
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

        // open/close without write doesn't change content - no rescan
        handle_for_test(Ok(vec![event]), &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn write_events_queue_record_aware_rescans() {
        register_test_media_providers();
        for kind in [
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            EventKind::Create(CreateKind::File),
        ] {
            let dir = TestDir::new("watch-test");
            let file = dir.join("track.flac");
            std::fs::write(&file, b"").unwrap();
            let (tx, mut rx) = channel(1);
            let event = DebouncedEvent::new(Event::new(kind).add_path(file), Instant::now());

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
                other => panic!("expected a record-aware rescan command, got {:?}", other),
            }
        }
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
