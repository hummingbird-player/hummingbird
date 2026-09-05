//! Subsonic protocol adapter. Wire-specific names and authentication stay here.
pub mod client;

mod catalog;
mod media;
mod normalize;
mod transcoding;
pub use catalog::SubsonicBackend;
mod reporting;

mod assets;
