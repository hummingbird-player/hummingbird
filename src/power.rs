#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

use gpui::{AppContext, Entity, Global};
use tracing::info;

use crate::playback::thread::PlaybackState;

#[cfg(target_os = "linux")]
use linux::PlatformPower;
#[cfg(target_os = "macos")]
use macos::PlatformPower;
#[cfg(target_os = "windows")]
use windows::PlatformPower;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
struct PlatformPower;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
impl PlatformPower {
    fn new() -> Self {
        Self
    }

    fn inhibit(&mut self) {}

    fn uninhibit(&mut self) {}
}

struct PowerManagerInner {
    platform: PlatformPower,
    playing: bool,
    prevent_idle: bool,
}

impl PowerManagerInner {
    fn new(prevent_idle: bool) -> Self {
        Self {
            platform: PlatformPower::new(),
            playing: false,
            prevent_idle,
        }
    }

    fn set_state(&mut self, state: PlaybackState) {
        let playing = state == PlaybackState::Playing;
        if self.playing == playing {
            return;
        }
        self.playing = playing;
        self.update();
    }

    fn set_prevent_idle(&mut self, prevent_idle: bool) {
        if self.prevent_idle == prevent_idle {
            return;
        }
        self.prevent_idle = prevent_idle;
        self.update();
    }

    fn update(&mut self) {
        if self.playing && self.prevent_idle {
            self.platform.inhibit();
        } else {
            self.platform.uninhibit();
        }
    }
}

#[derive(Clone)]
pub struct PowerManager(Entity<PowerManagerInner>);

impl Global for PowerManager {}

impl PowerManager {
    pub fn new(cx: &mut gpui::App, prevent_idle: bool) -> Self {
        Self(cx.new(|_| PowerManagerInner::new(prevent_idle)))
    }

    pub fn set_state<C: AppContext>(&self, cx: &mut C, state: PlaybackState) {
        self.0.update(cx, |inner, _| inner.set_state(state));
    }

    pub fn set_prevent_idle<C: AppContext>(&self, cx: &mut C, prevent_idle: bool) {
        self.0
            .update(cx, |inner, _| inner.set_prevent_idle(prevent_idle));
    }
}
