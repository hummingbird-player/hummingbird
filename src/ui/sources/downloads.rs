//! UI ownership of bounded, cancellable host download jobs. No job starts a
//! playback session; source cancellation remains enforced by the host resolver.
use super::SourceModels;
use crate::{
    sources::{
        SourceId, TrackRef,
        backend::{BackendError, BackendErrorKind},
    },
    toasts::{Toast, emit_toast},
};
use cntp_i18n::tr;
use gpui::{App, SharedString};

pub struct DownloadJob {
    id: u128,
    abort: tokio::task::AbortHandle,
}
impl Drop for DownloadJob {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

pub fn start(reference: TrackRef, title: SharedString, cx: &mut App) {
    let models = cx.global::<SourceModels>().clone();
    if models.downloads.read(cx).contains_key(&reference) {
        return;
    }
    if models.downloads.read(cx).len() >= 32 {
        emit_toast(Toast::error(tr!(
            "SOURCE_DOWNLOAD_QUEUE_FULL",
            "The download queue is full. Wait for a download to finish or cancel one."
        )));
        return;
    }
    let media = models.media.clone();
    let location = reference.clone();
    let task = crate::RUNTIME.spawn(async move { media.download(location).await });
    let id = rand::random();
    models.downloads.update(cx, |jobs, cx| {
        jobs.insert(
            reference.clone(),
            DownloadJob {
                id,
                abort: task.abort_handle(),
            },
        );
        cx.notify();
    });
    emit_toast(Toast::info(tr!(
        "SOURCE_DOWNLOAD_QUEUED",
        "Download queued: {{title}}",
        title = title.clone()
    )));
    cx.spawn(async move |cx| {
        let result = task.await;
        let current = models.downloads.update(cx, |jobs, cx| {
            if !jobs.get(&reference).is_some_and(|job| job.id == id) {
                return false;
            }
            jobs.remove(&reference);
            cx.notify();
            true
        });
        if !current {
            return;
        }
        match result {
            Ok(Ok(())) => emit_toast(Toast::success(tr!(
                "SOURCE_DOWNLOAD_COMPLETE",
                "Available offline: {{title}}",
                title = title
            ))),
            Ok(Err(error))
                if matches!(
                    error.kind,
                    BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration
                ) =>
            {
                ()
            }
            Ok(Err(error)) => failed(error),
            Err(error) if error.is_cancelled() => (),
            Err(_) => failed(BackendError::new(BackendErrorKind::Storage)),
        }
    })
    .detach();
}
pub fn cancel(reference: &TrackRef, cx: &mut App) {
    cx.global::<SourceModels>()
        .downloads
        .clone()
        .update(cx, |jobs, cx| {
            jobs.remove(reference);
            cx.notify();
        });
}
pub fn cancel_source(source: &SourceId, cx: &mut App) {
    cx.global::<SourceModels>()
        .downloads
        .clone()
        .update(cx, |jobs, cx| {
            jobs.retain(|reference, _| reference.source() != source);
            cx.notify();
        });
}
pub fn remove(reference: TrackRef, cx: &mut App) {
    cancel(&reference, cx);
    let media = cx.global::<SourceModels>().media.clone();
    crate::RUNTIME.spawn(async move {
        let result = async { media.cache().await?.remove_download(&reference).await }.await;
        cleared(result);
    });
}
pub fn reveal(reference: TrackRef, cx: &mut App) {
    let media = cx.global::<SourceModels>().media.clone();
    let task = crate::RUNTIME.spawn(async move { media.completed(&reference).await });
    cx.spawn(async move |cx| match task.await {
        Ok(Ok(Some(cached))) => {
            let path = cached.path();
            cx.update(|cx| crate::ui::util::reveal_path_for_file_manager(&path, cx));
        }
        Ok(Err(error)) => failed(error),
        _ => failed(BackendError::new(BackendErrorKind::NotFound)),
    })
    .detach();
}
pub fn clear_source(source: SourceId, include_offline: bool, cx: &mut App) {
    if include_offline {
        cancel_source(&source, cx);
    }
    let media = cx.global::<SourceModels>().media.clone();
    crate::RUNTIME.spawn(async move {
        let result = async { media.cache().await?.clear(&source, include_offline).await }.await;
        cleared(result);
    });
}
fn cleared(result: Result<u64, BackendError>) {
    match result {
        Ok(0) => emit_toast(Toast::success(tr!(
            "SOURCE_CACHE_CLEARED",
            "Cached music removed."
        ))),
        Ok(_) => emit_toast(Toast::info(tr!(
            "SOURCE_CACHE_CLEAR_DEFERRED",
            "Cached music removed. Files currently in use will be deleted when playback releases them."
        ))),
        Err(error) => failed(error),
    }
}
fn failed(error: BackendError) {
    emit_toast(Toast::error(tr!(
        "SOURCE_DOWNLOAD_FAILED",
        "Download or cache operation failed: {{details}}",
        details = super::error_text(&error)
    )));
}
