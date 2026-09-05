//! Library identity and host boundaries, available even in offline builds.
//!
//! Backends describe catalogs and resources; the host owns persistence, credentials,
//! scheduling, and media decoding. No backend DTO should depend on GPUI or a codec.
pub mod assets;
pub mod backend;
pub mod cache;
pub mod config;
pub mod credentials;
mod database;
#[cfg(feature = "online")]
pub mod http;
pub mod identity;
pub mod playback;
pub mod playlist_reference;
pub mod registry;
pub mod reporting;
pub mod resources;
pub mod service;
#[cfg(feature = "online")]
pub mod subsonic;
pub mod sync;
pub use identity::{SourceId, TrackLocation, TrackRef};

/// Local input availability. Remote resolution is supplied by the source registry.
/// Call only on workers: unlike the UI availability snapshot this probes the disk.
pub fn is_playable(reference: &TrackRef) -> bool {
    match reference.location() {
        TrackLocation::Local(path) => path.exists(),
        TrackLocation::Remote(_) => false,
    }
}

#[cfg(test)]
mod migration_tests;
