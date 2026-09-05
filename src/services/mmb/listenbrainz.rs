use client::ListenBrainzClient;
use gpui::SharedString;
use types::Session;

pub mod client;
pub mod types;

pub const MMBS_KEY: &str = "listenbrainz";

#[derive(Clone)]
pub enum ListenBrainzState {
    Disconnected { error: Option<SharedString> },
    Connected(Session),
}

pub type ListenBrainz = super::direct::DirectScrobbler<ListenBrainzClient>;
