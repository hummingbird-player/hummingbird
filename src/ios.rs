use std::{
    path::Path,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use block2::RcBlock;
use gpui_ios::{StatusBarContentStyle, ios::set_status_bar_style};
use objc2::MainThreadMarker;
use objc2_avf_audio::{
    AVAudioSession, AVAudioSessionCategoryPlayback, AVAudioSessionInterruptionNotification,
    AVAudioSessionInterruptionOptionKey, AVAudioSessionInterruptionOptions,
    AVAudioSessionInterruptionType, AVAudioSessionInterruptionTypeKey,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSNumber, NSString};
use objc2_ui_kit::{UIApplication, UIUserInterfaceStyle, UIWindowScene};
use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::{
    paths::container::{MARKER, container_id, is_container_id, relocate},
    services::controllers::ControllerBridge,
    ui::theme::Theme,
};

/// Puts the app's audio session in the playback category.
///
/// Without this the app uses the default (solo ambient) category, which iOS silences when the
/// ring/silent switch is set to silent and which stops when the screen locks.
pub fn configure_audio_session() {
    // SAFETY: the shared session is a process-wide singleton and these calls have no
    // preconditions beyond being made from a thread with an autorelease pool, which objc2 provides
    unsafe {
        let session = AVAudioSession::sharedInstance();
        let Some(category) = AVAudioSessionCategoryPlayback else {
            warn!(
                "AVAudioSessionCategoryPlayback is unavailable; audio may follow the silent switch"
            );
            return;
        };
        if let Err(error) = session.setCategory_error(category) {
            warn!("Couldn't set the audio session category: {error:?}");
        }
        if let Err(error) = session.setActive_error(true) {
            warn!("Couldn't activate the audio session: {error:?}");
        }
    }
}

/// Pauses and resumes playback around audio session interruptions (phone calls, Siri, alarms,
/// another app taking exclusive audio).
///
/// When an interruption begins, iOS deactivates the session and stops the output unit without
/// telling the playback thread, so the app would still think it's playing. Pausing keeps the UI and
/// lock screen honest. When the interruption ends the session has to be reactivated before output
/// works again, and playback resumes if it was playing beforehand and iOS says resuming is
/// appropriate.
///
/// `playing` must track whether playback is currently playing.
pub fn observe_audio_interruptions(bridge: ControllerBridge, playing: Arc<AtomicBool>) {
    // SAFETY: these are constant NSString statics exported by AVFAudio
    let (Some(name), Some(type_key), Some(option_key)) = (unsafe {
        (
            AVAudioSessionInterruptionNotification,
            AVAudioSessionInterruptionTypeKey,
            AVAudioSessionInterruptionOptionKey,
        )
    }) else {
        warn!("AVAudioSession interruption keys are unavailable; interruptions won't be handled");
        return;
    };

    let resume_after = AtomicBool::new(false);
    let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
        // SAFETY: the notification center passes a valid notification for the block's duration
        let notification = unsafe { notification.as_ref() };
        let Some(info) = notification.userInfo() else {
            return;
        };
        let number = |key: &NSString| {
            info.objectForKey(key)
                .and_then(|value| value.downcast::<NSNumber>().ok())
                .map(|value| value.unsignedIntegerValue())
        };
        let Some(kind) = number(type_key).map(AVAudioSessionInterruptionType) else {
            return;
        };

        if kind == AVAudioSessionInterruptionType::Began {
            let was_playing = playing.load(Ordering::Relaxed);
            info!("Audio session interrupted (was playing: {was_playing})");
            resume_after.store(was_playing, Ordering::Relaxed);
            if was_playing {
                bridge.pause();
            }
        } else if kind == AVAudioSessionInterruptionType::Ended {
            let options = AVAudioSessionInterruptionOptions(number(option_key).unwrap_or_default());
            let should_resume = options.contains(AVAudioSessionInterruptionOptions::ShouldResume);
            info!("Audio session interruption ended (should resume: {should_resume})");

            // SAFETY: as in configure_audio_session
            if let Err(error) = unsafe { AVAudioSession::sharedInstance().setActive_error(true) } {
                warn!("Couldn't reactivate the audio session after an interruption: {error:?}");
                return;
            }
            if resume_after.swap(false, Ordering::Relaxed) && should_resume {
                bridge.play();
            }
        }
    });

    // SAFETY: the block only touches thread-safe state, so it can run on whichever thread posts
    // the notification (queue: None)
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(name),
            None,
            None,
            &block,
        )
    };
    // the observer lives for the rest of the process
    std::mem::forget(observer);
}

/// Tells UIKit whether the selected theme is light or dark, so the status bar, home indicator and
/// system UI (keyboard, menus) are drawn in a contrasting color instead of the system default.
///
/// Must be called on the main thread, and again whenever the theme changes or a window opens, since
/// the interface style is overridden per window.
pub fn apply_theme_appearance(theme: &Theme) {
    let Some(main_thread) = MainThreadMarker::new() else {
        warn!("apply_theme_appearance called off the main thread; ignoring");
        return;
    };

    // judge by the text color rather than a background, since several backgrounds are in use:
    // light text means the theme is dark
    let text = theme.text;
    let luminance = 0.2126 * text.r + 0.7152 * text.g + 0.0722 * text.b;
    let dark = luminance > 0.5;

    set_status_bar_style(if dark {
        StatusBarContentStyle::Light
    } else {
        StatusBarContentStyle::Dark
    });

    let style = if dark {
        UIUserInterfaceStyle::Dark
    } else {
        UIUserInterfaceStyle::Light
    };
    let application = UIApplication::sharedApplication(main_thread);
    for scene in application.connectedScenes().iter() {
        let Ok(scene) = scene.downcast::<UIWindowScene>() else {
            continue;
        };
        for window in scene.windows().iter() {
            window.setOverrideUserInterfaceStyle(style);
        }
    }
}

/// The UUID of the data container this launch is running in.
fn current_container_id() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let id = container_id(&home).map(ToOwned::to_owned);
    if id.is_none() {
        warn!("Couldn't find the app container in $HOME ({home}); saved paths won't be relocated");
    }
    id
}

/// Rewrites paths saved by a previous install into the current data container in the JSON files
/// that hold them. Must run before any of those files are loaded.
///
/// Returns whether anything was relocated.
pub fn relocate_data_files(data_dir: &Path) -> bool {
    let Some(current_id) = current_container_id() else {
        return false;
    };

    let mut relocated = false;
    for name in ["settings.json", "playback_session.json", "app_data.json"] {
        let path = data_dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(updated) = relocate(&text, &current_id) else {
            continue;
        };

        let temp_path = path.with_extension("json.relocating");
        let result =
            std::fs::write(&temp_path, updated).and_then(|()| std::fs::rename(&temp_path, &path));
        match result {
            Ok(()) => {
                info!("Relocated saved paths in {name} to container {current_id}");
                relocated = true;
            }
            Err(error) => warn!("Couldn't relocate saved paths in {name}: {error}"),
        }
    }
    relocated
}

/// Rewrites track and album folder paths saved by a previous install into the current data
/// container.
///
/// Returns whether anything was relocated.
pub async fn relocate_library_paths(pool: &SqlitePool) -> bool {
    let Some(current_id) = current_container_id() else {
        return false;
    };

    let ids: Vec<String> = match sqlx::query_scalar(
        "SELECT DISTINCT substr(location, instr(location, ?1) + length(?1), 36) FROM track
             WHERE instr(location, ?1) > 0
         UNION
         SELECT DISTINCT substr(path, instr(path, ?1) + length(?1), 36) FROM album_path
             WHERE instr(path, ?1) > 0",
    )
    .bind(MARKER)
    .fetch_all(pool)
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            warn!("Couldn't look for library paths from a previous install: {error}");
            return false;
        }
    };

    let new_prefix = format!("{MARKER}{current_id}");
    let mut relocated = false;
    for old_id in ids
        .iter()
        .filter(|id| is_container_id(id) && **id != current_id)
    {
        let old_prefix = format!("{MARKER}{old_id}");
        let result: sqlx::Result<()> = async {
            let mut transaction = pool.begin().await?;
            // OR IGNORE: a scan since the reinstall may already have added a track at its new
            // location; the stale row is then left for the scanner to clean up as missing
            sqlx::query(
                "UPDATE OR IGNORE track
                 SET location = replace(location, ?1, ?2), folder = replace(folder, ?1, ?2)
                 WHERE instr(location, ?1) > 0 OR instr(folder, ?1) > 0",
            )
            .bind(&old_prefix)
            .bind(&new_prefix)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "UPDATE OR IGNORE album_path SET path = replace(path, ?1, ?2)
                 WHERE instr(path, ?1) > 0",
            )
            .bind(&old_prefix)
            .bind(&new_prefix)
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await
        }
        .await;

        match result {
            Ok(()) => {
                info!("Relocated library paths from container {old_id} to {current_id}");
                relocated = true;
            }
            Err(error) => warn!("Couldn't relocate library paths from container {old_id}: {error}"),
        }
    }
    relocated
}

/// Deletes the scan record, whose paths are stale after a relocation, so the next scan rebuilds it.
pub fn discard_scan_record(data_dir: &Path) {
    for name in ["scan_record.hsr", "scan_record_checkpoint.hsr"] {
        if let Err(error) = std::fs::remove_file(data_dir.join(name))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!("Couldn't delete {name} after relocating paths: {error}");
        }
    }
}
