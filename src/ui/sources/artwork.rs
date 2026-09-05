//! One cancellable now-playing artwork request, shared with the library cache.
use crate::{
    sources::{
        TrackRef,
        assets::{ArtworkTarget, Assets},
    },
    ui::models::{ImageEvent, Models, PlaybackInfo},
};
use gpui::{App, AppContext, Context, Entity};
use std::sync::Arc;

pub(super) struct NowPlayingArtwork {
    assets: Arc<Assets>,
    generation: u64,
    task: Option<tokio::task::AbortHandle>,
    binding: Option<(TrackRef, (String, bool))>,
}
impl Drop for NowPlayingArtwork {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
pub(super) fn initialize(assets: Arc<Assets>, cx: &mut App) -> Entity<NowPlayingArtwork> {
    cx.new(|cx| {
        let current = cx.global::<PlaybackInfo>().current_track.clone();
        cx.observe(&current, |this: &mut NowPlayingArtwork, _, cx| {
            this.refresh(true, cx)
        })
        .detach();
        let library = cx.global::<Models>().library_change.clone();
        let mut completed = library.read(cx).completed;
        cx.observe(
            &library,
            move |this: &mut NowPlayingArtwork, library, cx| {
                if library.read(cx).take_completion(&mut completed) {
                    this.refresh(true, cx);
                }
            },
        )
        .detach();
        let mut this = NowPlayingArtwork {
            assets,
            generation: 0,
            task: None,
            binding: None,
        };
        this.refresh(true, cx);
        this
    })
}
impl NowPlayingArtwork {
    pub(super) fn refresh(&mut self, force: bool, cx: &mut Context<Self>) {
        let reference = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .as_ref()
            .map(|track| track.get_track_ref().clone());
        let binding = reference.and_then(|reference| {
            if reference.source().is_local() {
                return None;
            }
            self.assets
                .display_binding(reference.source())
                .map(|account| (reference, account))
        });
        if !force && self.binding == binding {
            return;
        }
        self.binding = binding.clone();
        self.generation = self.generation.wrapping_add(1);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let Some((reference, (account, _))) = binding else {
            return;
        };
        let assets = self.assets.clone();
        let generation = self.generation;
        let task = crate::RUNTIME.spawn(async move {
            assets
                .artwork(ArtworkTarget::Reference(reference), false)
                .await
        });
        self.task = Some(task.abort_handle());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(bytes))) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                this.task = None;
                let Some((reference, _)) = &this.binding else {
                    return;
                };
                if !this
                    .assets
                    .account_key(reference.source())
                    .is_some_and(|current| current == account)
                {
                    return;
                }
                let models = cx.global::<Models>();
                let art = models.albumart.clone();
                let original = models.albumart_original.clone();
                art.update(cx, |_, cx| {
                    cx.emit(ImageEvent(bytes.clone().into_boxed_slice()))
                });
                original.update(cx, |_, cx| cx.emit(ImageEvent(bytes.into_boxed_slice())));
            });
        })
        .detach();
    }
}
