use client::LastFMClient;
use gpui::SharedString;
use std::sync::LazyLock;
use types::Session;

pub mod client;
pub mod types;

pub const MMBS_KEY: &str = "lastfm";

#[derive(Clone)]
pub enum LastFMState {
    Disconnected { error: Option<SharedString> },
    AwaitingFinalization(String),
    Connected(Session),
}

pub fn is_available() -> bool {
    LASTFM_CREDS.is_some()
}

pub static LASTFM_CREDS: LazyLock<Option<(&str, &str)>> = LazyLock::new(|| {
    let key = std::env::var("LASTFM_API_KEY")
        .map_or(None, |k| Some(&*k.leak()))
        .or(option_env!("LASTFM_API_KEY"))?;
    let secret = std::env::var("LASTFM_API_SECRET")
        .map_or(None, |k| Some(&*k.leak()))
        .or(option_env!("LASTFM_API_SECRET"))?;
    Some((key, secret))
});

pub type LastFM = super::direct::DirectScrobbler<LastFMClient>;
