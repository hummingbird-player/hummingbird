use super::{
    client::{SubsonicClient, malformed},
    normalize::{self, array, boolean, id, text},
};
use crate::sources::backend::*;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, VecDeque},
    io,
    sync::{Arc, Mutex, RwLock},
};

const MAX_CURSOR: usize = 4 * 1024 * 1024;
const ALBUM_BATCH: usize = 50;
/// One bounded wire response is retained while splitting large albums/directories.
/// It is connection-local and is never persisted in settings or the checkpoint.
pub struct SubsonicBackend {
    client: Arc<SubsonicClient>,
    info: RwLock<Option<BackendInfo>>,
    extensions: RwLock<BTreeSet<String>>,
    response_cache: Mutex<Option<(String, Arc<Value>)>>,
    resources: crate::sources::resources::ResourceTable,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
enum Step {
    AlbumList {
        folder: String,
        offset: u32,
    },
    Album {
        id: String,
        offset: usize,
        signature: Option<String>,
    },
    Roots {
        folder: String,
        offset: usize,
        signature: Option<String>,
    },
    Directory {
        id: String,
        offset: usize,
        signature: Option<String>,
    },
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    version: u8,
    folders: Vec<String>,
    scope: Option<String>,
    queue: VecDeque<Step>,
    directories: BTreeSet<String>,
    album_pages: BTreeSet<String>,
    complete: bool,
    requests: u32,
}
impl SubsonicBackend {
    pub fn new(client: SubsonicClient) -> Self {
        Self {
            client: Arc::new(client),
            info: RwLock::new(None),
            extensions: RwLock::new(BTreeSet::new()),
            response_cache: Mutex::new(None),
            resources: Default::default(),
        }
    }
    async fn cached(
        &self,
        endpoint: &str,
        parameters: &[(&str, String)],
    ) -> BackendResult<Arc<Value>> {
        let key = serde_json::to_string(&(endpoint, parameters)).map_err(|_| malformed())?;
        if let Some((previous, response)) = &*self
            .response_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            if *previous == key {
                return Ok(response.clone());
            }
        }
        let response = Arc::new(self.client.json(endpoint, parameters).await?);
        *self
            .response_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((key, response.clone()));
        Ok(response)
    }
    fn connection(&self) -> BackendResult<BackendInfo> {
        self.info
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| BackendError::new(BackendErrorKind::Network))
    }
    async fn page(&self, request: CatalogRequest) -> BackendResult<CatalogPage> {
        let info = self.connection()?;
        let bandcamp = info.server_name.eq_ignore_ascii_case("BandcampServer");
        let mut folders = request.folder_ids;
        folders.sort();
        folders.dedup();
        if folders.is_empty() {
            folders = info
                .folders
                .iter()
                .map(|folder| folder.id.clone())
                .collect();
            folders.sort();
            folders.dedup();
        }
        if folders
            .iter()
            .any(|id| !info.folders.iter().any(|folder| folder.id == *id))
        {
            return Err(malformed());
        }
        let mut cursor = if let Some(cursor) = request.cursor {
            if cursor.len() > MAX_CURSOR {
                return Err(limit());
            }
            let cursor: Cursor = serde_json::from_str(&cursor).map_err(|_| malformed())?;
            if cursor.version != 1 || cursor.folders != folders || cursor.scope != info.scope_token
            {
                return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
            }
            cursor
        } else {
            let queue = folders
                .iter()
                .flat_map(|folder| {
                    let mut steps = vec![Step::AlbumList {
                        folder: folder.clone(),
                        offset: 0,
                    }];
                    // Bandcamp exposes its synthetic Collection folder for client
                    // compatibility, but its catalog is album-based and does not
                    // reliably implement the legacy directory traversal endpoints.
                    if !bandcamp {
                        steps.push(Step::Roots {
                            folder: folder.clone(),
                            offset: 0,
                            signature: None,
                        });
                    }
                    steps
                })
                .collect();
            Cursor {
                version: 1,
                folders,
                scope: info.scope_token.clone(),
                queue,
                directories: BTreeSet::new(),
                album_pages: BTreeSet::new(),
                complete: true,
                requests: 0,
            }
        };
        if cursor.requests >= 2_000_000
            || cursor.queue.len() > 100_000
            || cursor.directories.len() > 100_000
            || cursor.album_pages.len() > 100_000
        {
            return Err(limit());
        }
        let mut page = CatalogPage {
            supplemental: false,
            tracks: vec![],
            albums: vec![],
            artists: vec![],
            next_cursor: None,
            completion: SnapshotCompletion::InProgress,
            scope_token: info.scope_token,
        };
        let size = usize::from(request.limit.clamp(1, 512));
        if let Some(step) = cursor.queue.pop_front() {
            cursor.requests += 1;
            match step {
                Step::AlbumList { folder, offset } => {
                    let response = self
                        .client
                        .json(
                            "getAlbumList2",
                            &[
                                ("type", "alphabeticalByName".into()),
                                ("size", ALBUM_BATCH.to_string()),
                                ("offset", offset.to_string()),
                                ("musicFolderId", folder.clone()),
                            ],
                        )
                        .await;
                    match response {
                        Ok(response) => {
                            let list = response
                                .get("albumList2")
                                .filter(|v| v.is_object())
                                .ok_or_else(malformed)?;
                            let albums = array(list, "album")?;
                            if albums.len() > ALBUM_BATCH {
                                return Err(limit());
                            }
                            if !albums.is_empty() {
                                let ids: Vec<String> =
                                    albums.iter().map(id).collect::<BackendResult<_>>()?;
                                let key = serde_json::to_string(&(&folder, ids))
                                    .map_err(|_| malformed())?;
                                if !cursor.album_pages.insert(digest(&key)?) {
                                    return Err(malformed());
                                }
                                if !bandcamp || albums.len() == ALBUM_BATCH {
                                    cursor.queue.push_front(Step::AlbumList {
                                        folder,
                                        offset: offset
                                            .checked_add(albums.len() as u32)
                                            .ok_or_else(limit)?,
                                    });
                                }
                                for album in albums.iter().rev() {
                                    cursor.queue.push_front(Step::Album {
                                        id: id(album)?,
                                        offset: 0,
                                        signature: None,
                                    });
                                }
                            }
                        }
                        // Indexed browsing is optional. Roots still enumerate all
                        // directories and loose songs for this selected folder.
                        Err(error) if optional_endpoint(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                Step::Album {
                    id: album_id,
                    offset,
                    signature,
                } => {
                    let response = self.cached("getAlbum", &[("id", album_id.clone())]).await;
                    match response {
                        Ok(response) => {
                            let album = response
                                .get("album")
                                .filter(|v| v.is_object())
                                .ok_or_else(malformed)?;
                            if id(album)? != album_id {
                                return Err(malformed());
                            }
                            let songs = array(album, "song")?;
                            let signature = verify_slice(album, offset, songs.len(), signature)?;
                            let end = offset.saturating_add(size).min(songs.len());
                            page.albums.push(normalize::album(album)?);
                            for song in &songs[offset..end] {
                                if let Some(mut track) = normalize::song(song, Some(&album_id))? {
                                    if track.disc_subtitle.is_none() {
                                        track.disc_subtitle = array(album, "discTitles")?
                                            .iter()
                                            .find(|disc| {
                                                normalize::number(&disc["disc"])
                                                    == track.disc_number.map(f64::from)
                                            })
                                            .and_then(|disc| text(disc, "title"));
                                    }
                                    page.tracks.push(track);
                                }
                            }
                            if end < songs.len() {
                                cursor.queue.push_front(Step::Album {
                                    id: album_id,
                                    offset: end,
                                    signature: Some(signature),
                                });
                            }
                        }
                        Err(error) if error.kind == BackendErrorKind::Unsupported => {}
                        Err(error) => return Err(error),
                    }
                }
                Step::Roots {
                    folder,
                    offset,
                    signature,
                } => {
                    page.supplemental = true;
                    match self
                        .cached("getIndexes", &[("musicFolderId", folder.clone())])
                        .await
                    {
                        Ok(response) => {
                            let indexes = response
                                .get("indexes")
                                .filter(|v| v.is_object())
                                .ok_or_else(malformed)?;
                            let children = array(indexes, "child")?;
                            let signature =
                                verify_slice(indexes, offset, children.len(), signature)?;
                            if offset == 0 {
                                for index in array(indexes, "index")? {
                                    for artist in array(index, "artist")? {
                                        enqueue_directory(&mut cursor, id(artist)?)?;
                                    }
                                }
                                for shortcut in array(indexes, "shortcut")? {
                                    enqueue_directory(&mut cursor, id(shortcut)?)?;
                                }
                            }
                            let end = offset.saturating_add(size).min(children.len());
                            import_children(&mut cursor, &mut page, &children[offset..end])?;
                            if end < children.len() {
                                cursor.queue.push_front(Step::Roots {
                                    folder,
                                    offset: end,
                                    signature: Some(signature),
                                });
                            }
                        }
                        // An indexed-only server cannot prove that it included all
                        // loose songs. Retain its data without deletion inference.
                        Err(error) if optional_endpoint(&error) => cursor.complete = false,
                        Err(error) => return Err(error),
                    }
                }
                Step::Directory {
                    id: directory_id,
                    offset,
                    signature,
                } => {
                    page.supplemental = true;
                    match self
                        .cached("getMusicDirectory", &[("id", directory_id.clone())])
                        .await
                    {
                        Ok(response) => {
                            let directory = response
                                .get("directory")
                                .filter(|v| v.is_object())
                                .ok_or_else(malformed)?;
                            if id(directory)? != directory_id {
                                return Err(malformed());
                            }
                            let children = array(directory, "child")?;
                            let signature =
                                verify_slice(directory, offset, children.len(), signature)?;
                            let end = offset.saturating_add(size).min(children.len());
                            import_children(&mut cursor, &mut page, &children[offset..end])?;
                            if end < children.len() {
                                cursor.queue.push_front(Step::Directory {
                                    id: directory_id,
                                    offset: end,
                                    signature: Some(signature),
                                });
                            }
                        }
                        // Missing directories are snapshot changes, not endpoint
                        // absence. Never downgrade 404 here and prune their songs.
                        Err(error) if error.kind == BackendErrorKind::Unsupported => {
                            cursor.complete = false
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if cursor.queue.is_empty() {
            page.completion = if cursor.complete {
                SnapshotCompletion::Authoritative
            } else {
                SnapshotCompletion::Additive
            };
        } else {
            let next = serde_json::to_string(&cursor).map_err(|_| malformed())?;
            if next.len() > MAX_CURSOR {
                return Err(limit());
            }
            page.next_cursor = Some(next);
        }
        Ok(page)
    }
}
fn optional_endpoint(error: &BackendError) -> bool {
    matches!(
        error.kind,
        BackendErrorKind::Unsupported | BackendErrorKind::NotFound
    )
}
fn limit() -> BackendError {
    BackendError::new(BackendErrorKind::ResourceLimit)
}
fn enqueue_directory(cursor: &mut Cursor, id: String) -> BackendResult<()> {
    if cursor.directories.len() >= 100_000 {
        return Err(limit());
    }
    // IDs are opaque. The visited set prevents cycles and duplicate shortcuts.
    if cursor.directories.insert(id.clone()) {
        cursor.queue.push_back(Step::Directory {
            id,
            offset: 0,
            signature: None,
        });
    }
    Ok(())
}
fn import_children(
    cursor: &mut Cursor,
    page: &mut CatalogPage,
    children: &[Value],
) -> BackendResult<()> {
    for child in children {
        if boolean(&child["isDir"]) == Some(true) {
            enqueue_directory(cursor, id(child)?)?;
        } else if let Some(track) = normalize::song(child, None)? {
            if let Some(album) = normalize::song_album(child) {
                if !page.albums.iter().any(|existing| existing.id == album.id) {
                    page.albums.push(album);
                }
            }
            page.tracks.push(track);
        }
    }
    Ok(())
}
fn digest(value: &impl Serialize) -> BackendResult<String> {
    struct Digest(md5::Context);
    impl io::Write for Digest {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.consume(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Digest(md5::Context::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| malformed())?;
    Ok(format!("{:x}", writer.0.finalize()))
}
fn verify_slice(
    value: &Value,
    offset: usize,
    length: usize,
    previous: Option<String>,
) -> BackendResult<String> {
    if offset > length || (offset > 0 && previous.is_none()) {
        return Err(malformed());
    }
    let signature = digest(value)?;
    if previous.is_some_and(|previous| previous != signature) {
        return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
    }
    Ok(signature)
}
#[async_trait]
impl LibraryBackend for SubsonicBackend {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        // Clear the old discovery before I/O so failed reconnects cannot reuse
        // stale permission/capability state.
        *self.info.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .response_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let ping = self.client.json("ping", &[]).await?;
        let extensions = match self.client.json("getOpenSubsonicExtensions", &[]).await {
            Ok(response) => {
                let values = response
                    .get("openSubsonicExtensions")
                    .and_then(Value::as_array)
                    .ok_or_else(malformed)?;
                let mut extensions = BTreeSet::new();
                for value in values {
                    if array(value, "versions")?
                        .iter()
                        .any(|version| normalize::number(version) == Some(1.0))
                    {
                        if let Some(name) = text(value, "name") {
                            extensions.insert(name);
                        }
                    }
                }
                extensions
            }
            Err(error) if optional_endpoint(&error) => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        if self.client.uses_api_key() && !extensions.contains("apiKeyAuthentication") {
            return Err(BackendError::unsupported());
        }
        let response = self.client.json("getMusicFolders", &[]).await?;
        let folders = response
            .get("musicFolders")
            .filter(|value| value.is_object())
            .ok_or_else(malformed)?;
        let folders: Vec<MusicFolder> = array(folders, "musicFolder")?
            .iter()
            .map(|folder| {
                Ok(MusicFolder {
                    id: id(folder)?,
                    name: text(folder, "name").unwrap_or_default(),
                })
            })
            .collect::<BackendResult<_>>()?;
        if folders.len() > 4096 {
            return Err(limit());
        }
        let mut capabilities = [
            Capability::Catalog,
            Capability::Artwork,
            Capability::Lyrics,
            Capability::OriginalMedia,
            Capability::Transcoding,
            Capability::NowPlaying,
            Capability::Scrobble,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        if text(&ping, "version")
            .as_deref()
            .is_some_and(super::reporting::supports_batch)
        {
            capabilities.insert(Capability::ScrobbleBatch);
        }
        if extensions.contains("transcoding") || extensions.contains("transcodeOffset") {
            capabilities.insert(Capability::OffsetSeeking);
        }
        if extensions.contains("playbackReport") {
            capabilities.insert(Capability::PlaybackReport);
        }
        let server_name = text(&ping, "type").unwrap_or_else(|| "Subsonic".into());
        let server_version = text(&ping, "serverVersion")
            .or_else(|| text(&ping, "version"))
            .unwrap_or_default();
        super::reporting::apply_compatibility(&server_name, &server_version, &mut capabilities);
        let info = BackendInfo {
            server_name,
            server_version,
            capabilities,
            folders,
            scope_token: None,
        };
        *self.extensions.write().unwrap_or_else(|e| e.into_inner()) = extensions;
        *self.info.write().unwrap_or_else(|e| e.into_inner()) = Some(info.clone());
        Ok(info)
    }
    async fn report_playback(&self, report: PlaybackReport) -> BackendResult<()> {
        let info = self.connection()?;
        super::reporting::send(&self.client, &info.capabilities, report).await
    }
    async fn catalog_page(&self, request: CatalogRequest) -> BackendResult<CatalogPage> {
        let result = self.page(request).await;
        if result
            .as_ref()
            .map_or(true, |page| page.next_cursor.is_none())
        {
            *self
                .response_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
        }
        result
    }
    async fn track(&self, location: &str) -> BackendResult<RemoteTrack> {
        let response = self
            .client
            .json("getSong", &[("id", location.into())])
            .await?;
        let song = response.get("song").ok_or_else(malformed)?;
        let track = normalize::song(song, None)?.ok_or_else(malformed)?;
        if track.id != location {
            return Err(malformed());
        }
        Ok(track)
    }
    async fn resolve_media(&self, request: MediaRequest) -> BackendResult<MediaDescriptor> {
        use super::media::{BinaryResource, MAX_MEDIA_BYTES, canonical_format};
        if request.location.is_empty() || request.location.len() > 4096 {
            return Err(malformed());
        }
        let permit = self.resources.reserve()?;
        let track = self.track(&request.location).await?;
        let extensions = self
            .extensions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut plan =
            super::transcoding::plan(&self.client, &extensions, &request, &track).await?;
        let mut resource = BinaryResource::open(
            self.client.clone(),
            plan.endpoint,
            std::mem::take(&mut plan.parameters),
            plan.original,
            MAX_MEDIA_BYTES,
        )
        .await?;
        let mut detected = resource.detected_format();
        if let (Some(actual), Some(expected)) = (detected, plan.format.as_deref()) {
            if actual != canonical_format(expected) {
                if !plan.can_reopen_original(actual, &request, &track) {
                    return Err(malformed());
                }
                // A returned source container does not prove whether timeOffset
                // was honored. Reopen raw at zero and let the codec perform the
                // requested seek rather than assigning an invented time origin.
                drop(resource);
                let mut original = request.clone();
                original.quality = QualityPolicy::Original;
                original.offset_ms = 0;
                plan =
                    super::transcoding::plan(&self.client, &extensions, &original, &track).await?;
                resource = BinaryResource::open(
                    self.client.clone(),
                    plan.endpoint,
                    std::mem::take(&mut plan.parameters),
                    true,
                    MAX_MEDIA_BYTES,
                )
                .await?;
                detected = resource.detected_format();
                if detected != Some(actual) {
                    return Err(malformed());
                }
            }
        }
        let revision = plan.cache_revision(resource.validator(), track.content_revision.as_deref());
        let descriptor = MediaDescriptor {
            resource: ResourceHandle(0),
            format: detected.map(str::to_owned).or(plan.format),
            exact_length: resource.length(),
            seek: if plan.offset_seeking {
                SeekSupport::TimeOffset
            } else {
                resource.seek_support()
            },
            expires_at_ms: None,
            timeline_offset_ms: plan.offset_ms,
            revision,
        };
        Ok(MediaDescriptor {
            resource: self.resources.insert(permit, Box::new(resource)),
            ..descriptor
        })
    }
    async fn resource(&self, request: ResourceRequest) -> BackendResult<ResourcePage> {
        self.connection()?;
        match request {
            ResourceRequest::Artwork { id, size } => {
                let permit = self.resources.reserve()?;
                let (bytes, mime) = super::assets::artwork(&self.client, &id, size).await?;
                let resource = self
                    .resources
                    .insert(permit, Box::new(super::assets::ImageBytes(bytes)));
                Ok(ResourcePage::Binary { resource, mime })
            }
            ResourceRequest::Lyrics { location } => {
                let structured = self
                    .extensions
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains("songLyrics");
                let document = super::assets::lyrics(&self.client, &location, structured).await?;
                Ok(ResourcePage::Lyrics { document })
            }
            _ => Err(BackendError::unsupported()),
        }
    }
    async fn read_resource(&self, request: ResourceRead) -> BackendResult<ResourceChunk> {
        self.resources.read(request).await
    }
    fn release_resource(&self, resource: ResourceHandle) {
        self.resources.release(resource);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod discovery_tests;
