//! Protocol-independent, owned boundary types. A future component/WASM adapter
//! implements this trait; it never receives database, UI, decoder, or secret handles.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Catalog,
    OriginalMedia,
    Transcoding,
    OffsetSeeking,
    Artwork,
    Lyrics,
    FavoritesRead,
    FavoritesWrite,
    RatingsWrite,
    PlaylistsRead,
    PlaylistsWrite,
    NowPlaying,
    Scrobble,
    ScrobbleBatch,
    PlaybackReport,
    Bookmarks,
    ServerQueue,
    Recommendations,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendInfo {
    pub server_name: String,
    pub server_version: String,
    /// Operations supported by this adapter after server protocol negotiation.
    pub capabilities: BTreeSet<Capability>,
    pub folders: Vec<MusicFolder>,
    /// Opaque account/permission scope. Changes invalidate deletion reconciliation.
    pub scope_token: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MusicFolder {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendErrorKind {
    Authentication,
    Forbidden,
    NotFound,
    Unsupported,
    Network,
    RateLimited,
    MalformedResponse,
    Cancelled,
    ResourceLimit,
    StaleConfiguration,
    Storage,
}

/// Only host-approved static messages are exposed. Do not put HTTP errors, response
/// bodies, request URLs, or authentication fields in an error's display/debug text.
#[derive(Clone, Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{kind:?}")]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub retry_after_ms: Option<u64>,
}
impl BackendError {
    pub fn new(kind: BackendErrorKind) -> Self {
        Self {
            kind,
            retry_after_ms: None,
        }
    }
    pub fn unsupported() -> Self {
        Self::new(BackendErrorKind::Unsupported)
    }
    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            BackendErrorKind::Network | BackendErrorKind::RateLimited
        )
    }
}
pub type BackendResult<T> = Result<T, BackendError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogRequest {
    pub cursor: Option<String>,
    pub folder_ids: Vec<String>,
    pub limit: u16,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotCompletion {
    InProgress,
    /// The complete selected account scope was enumerated, including loose songs.
    Authoritative,
    /// Useful indexed data, but not evidence that unseen tracks were deleted.
    Additive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogPage {
    /// Supplemental enumeration fills gaps without downgrading records already seen in this pass.
    #[serde(default)]
    pub supplemental: bool,
    pub tracks: Vec<RemoteTrack>,
    pub albums: Vec<RemoteAlbum>,
    pub artists: Vec<RemoteArtist>,
    pub next_cursor: Option<String>,
    pub completion: SnapshotCompletion,
    pub scope_token: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteArtist {
    pub id: String,
    pub name: String,
    pub sort_name: Option<String>,
    pub musicbrainz_id: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteAlbum {
    pub id: String,
    pub title: String,
    pub sort_title: Option<String>,
    pub artist_display: Option<String>,
    pub artists: Option<Vec<RemoteArtist>>,
    pub release_date: Option<ReleaseDate>,
    pub musicbrainz_id: Option<String>,
    pub artwork: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseDate {
    pub year: i32,
    pub month: Option<u8>,
    pub day: Option<u8>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReplayGain {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteTrack {
    pub id: String,
    pub title: String,
    pub sort_title: Option<String>,
    pub album_id: Option<String>,
    /// Distinguishes a known albumless song from missing album metadata.
    pub album_known: bool,
    pub artist_display: Option<String>,
    pub artists: Option<Vec<RemoteArtist>>,
    pub genres: Option<Vec<String>>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_subtitle: Option<String>,
    pub duration_ms: Option<u64>,
    pub release_date: Option<ReleaseDate>,
    pub musicbrainz_id: Option<String>,
    pub replay_gain: ReplayGain,
    pub artwork: Option<String>,
    pub lyrics: Option<String>,
    pub starred: Option<bool>,
    pub rating: Option<u8>,
    pub content_revision: Option<String>,
    pub original_format: Option<String>,
    pub original_bitrate_kbps: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum QualityPolicy {
    #[default]
    Original,
    Automatic,
    Transcode {
        format: String,
        bitrate_kbps: u32,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaRequest {
    /// A single Automatic retry after the host decoder rejects the first input.
    /// Backends must select a transcode or return Unsupported, never direct play.
    #[serde(default)]
    pub force_transcode: bool,
    pub location: String,
    pub quality: QualityPolicy,
    /// Desired global position. A byte-seekable original can return origin zero;
    /// a time-offset transcode returns its actual (possibly rounded) origin.
    pub offset_ms: u64,
    pub supported_formats: Vec<String>,
    #[serde(default)]
    pub decode_profiles: Vec<crate::media::capabilities::AudioDecodeProfile>,
}
/// A host resource table key. It is never an authenticated or signed URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceHandle(pub u64);
/// The host bounds each byte transfer. Native adapters move bytes directly; a
/// component adapter can expose a byte stream rather than JSON-encoding media.
pub const MAX_RESOURCE_READ: u32 = 256 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceRead {
    pub resource: ResourceHandle,
    pub offset: u64,
    pub max_bytes: u32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ResourceChunk {
    pub offset: u64,
    pub bytes: Vec<u8>,
    /// Only a validated end of the resource, never a temporary empty buffer.
    pub eof: bool,
}
impl std::fmt::Debug for ResourceChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceChunk")
            .field("offset", &self.offset)
            .field("length", &self.bytes.len())
            .field("eof", &self.eof)
            .finish()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeekSupport {
    None,
    Cached,
    ByteRange,
    TimeOffset,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaDescriptor {
    pub resource: ResourceHandle,
    pub format: Option<String>,
    pub exact_length: Option<u64>,
    pub seek: SeekSupport,
    pub expires_at_ms: Option<u64>,
    pub timeline_offset_ms: u64,
    pub revision: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceRequest {
    Artwork {
        id: String,
        size: Option<u32>,
    },
    Lyrics {
        location: String,
    },
    ArtistInfo {
        id: String,
    },
    AlbumInfo {
        id: String,
    },
    Playlists {
        cursor: Option<String>,
    },
    Playlist {
        id: String,
    },
    Recommendations {
        location: Option<String>,
        limit: u16,
    },
}
/// Normalized lyric lines remain owned data across a future component boundary.
/// Host/UI code need not parse protocol-specific text formats or endpoint names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsMatch {
    TrackId,
    Metadata,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub start_ms: Option<u64>,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricsDocument {
    pub language: Option<String>,
    pub matched_by: LyricsMatch,
    pub lines: Vec<LyricLine>,
}
pub const MAX_LYRICS_BYTES: usize = 1024 * 1024;
pub const MAX_LYRIC_LINES: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourcePage {
    Binary {
        resource: ResourceHandle,
        mime: String,
    },
    Lyrics {
        document: LyricsDocument,
    },
    Information {
        text: String,
    },
    Playlists {
        items: Vec<RemotePlaylist>,
        next_cursor: Option<String>,
    },
    Tracks {
        items: Vec<RemoteTrack>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemotePlaylist {
    pub id: String,
    pub name: String,
    pub writable: bool,
    pub revision: Option<String>,
    pub tracks: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibraryMutation {
    Favorite {
        location: String,
        value: bool,
    },
    Rating {
        location: String,
        value: u8,
    },
    SavePlaylist {
        id: Option<String>,
        name: String,
        tracks: Vec<String>,
        expected_revision: Option<String>,
    },
    DeletePlaylist {
        id: String,
        expected_revision: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MutationResult {
    pub id: Option<String>,
    pub revision: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackReportState {
    Starting,
    Playing,
    Paused,
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenReport {
    pub location: String,
    pub started_at_ms: i64,
}
pub const MAX_REPORT_BATCH: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaybackReport {
    NowPlaying {
        location: String,
        started_at_ms: i64,
    },
    Listen {
        location: String,
        started_at_ms: i64,
    },
    Listens {
        listens: Vec<ListenReport>,
    },
    State {
        location: String,
        position_ms: u64,
        state: PlaybackReportState,
        rate: f64,
        ignore_scrobble: bool,
    },
}

/// Optional methods fail explicitly. Minimal sources (e.g. WebDAV) implement only
/// catalog/media operations, without pretending to be playback-reporting services.
#[async_trait]
pub trait LibraryBackend: Send + Sync {
    async fn connect(&self) -> BackendResult<BackendInfo>;
    async fn catalog_page(&self, request: CatalogRequest) -> BackendResult<CatalogPage>;
    async fn track(&self, location: &str) -> BackendResult<RemoteTrack>;
    async fn resolve_media(&self, request: MediaRequest) -> BackendResult<MediaDescriptor>;
    async fn read_resource(&self, _request: ResourceRead) -> BackendResult<ResourceChunk> {
        Err(BackendError::unsupported())
    }
    /// Local cleanup, including cancellation of an in-flight read. Must remain
    /// callable after a source lease has been cancelled; it sends no request.
    fn release_resource(&self, _resource: ResourceHandle) {}
    async fn resource(&self, _request: ResourceRequest) -> BackendResult<ResourcePage> {
        Err(BackendError::unsupported())
    }
    async fn mutate(&self, _mutation: LibraryMutation) -> BackendResult<MutationResult> {
        Err(BackendError::unsupported())
    }
    async fn report_playback(&self, _report: PlaybackReport) -> BackendResult<()> {
        Err(BackendError::unsupported())
    }
}
