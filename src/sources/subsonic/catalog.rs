//! Catalog requests and normalization into Hummingbird's source-neutral metadata model.

use serde::Deserialize;

use crate::{
    media::metadata::{Metadata, MetadataTag, apply_tag},
    sources::{
        BackendError, CatalogPage, CatalogRequest, RemoteAlbum, RemoteAlbumRef, RemoteTrack,
    },
};

use super::client::SubsonicClient;

pub(super) async fn catalog_page(
    client: &SubsonicClient,
    request: CatalogRequest,
) -> Result<CatalogPage, BackendError> {
    let offset = request
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| BackendError::InvalidRequest)?;
    let page_size = request.page_size.clamp(1, 500);
    let response = client
        .request_with_params::<AlbumListPayload>(
            "getAlbumList2.view",
            true,
            &[
                ("type", "alphabeticalByName".into()),
                ("size", page_size.to_string()),
                ("offset", offset.to_string()),
            ],
        )
        .await?;
    let albums = response
        .payload
        .album_list
        .ok_or(BackendError::MalformedResponse)?
        .albums
        .into_iter()
        .map(|album| RemoteAlbumRef { location: album.id })
        .collect::<Vec<_>>();
    let next_cursor = if albums.len() == page_size {
        Some(
            offset
                .checked_add(albums.len())
                .ok_or(BackendError::MalformedResponse)?
                .to_string(),
        )
    } else {
        None
    };
    Ok(CatalogPage {
        albums,
        next_cursor,
    })
}

pub(super) async fn album(
    client: &SubsonicClient,
    album: &RemoteAlbumRef,
) -> Result<RemoteAlbum, BackendError> {
    if album.location.is_empty() {
        return Err(BackendError::InvalidRequest);
    }
    let response = client
        .request_with_params::<AlbumPayload>(
            "getAlbum.view",
            true,
            &[("id", album.location.clone())],
        )
        .await?;
    let album_response = response
        .payload
        .album
        .ok_or(BackendError::MalformedResponse)?;
    normalize_album(album_response, &album.location)
}

fn normalize_album(album: AlbumResponse, expected_id: &str) -> Result<RemoteAlbum, BackendError> {
    if album.id != expected_id || album.id.is_empty() || album.name.trim().is_empty() {
        return Err(BackendError::MalformedResponse);
    }

    let album_artists = contributor_names(album.album_artists);
    let album_artist = album
        .artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| album_artists.first().cloned());
    let mut metadata = Metadata {
        name: Some(album.name.clone()),
        album: Some(album.name.clone()),
        sort_album: album.sort_name,
        artist: album_artist.clone(),
        album_artist: album_artist.clone(),
        year: album.year.and_then(|year| u16::try_from(year).ok()),
        mbid_album: album.music_brainz_id,
        ..Metadata::default()
    };
    metadata.album_artist_keys.extend(album_artists);
    if metadata.album_artist_keys.is_empty()
        && let Some(artist) = album_artist
    {
        metadata.album_artist_keys.push(artist);
    }
    add_genres(&mut metadata, album.genre, album.genres);

    let tracks = album
        .songs
        .into_iter()
        .filter(|song| !song.is_dir && !song.is_video)
        .map(|song| normalize_song(song, &album.name, &metadata))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RemoteAlbum {
        location: album.id,
        metadata,
        tracks,
    })
}

fn normalize_song(
    song: Song,
    album_name: &str,
    album_metadata: &Metadata,
) -> Result<RemoteTrack, BackendError> {
    if song.id.is_empty() || song.title.trim().is_empty() {
        return Err(BackendError::MalformedResponse);
    }

    let artists = contributor_names(song.artists);
    let album_artists = contributor_names(song.album_artists);
    let artist = song
        .artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| artists.first().cloned());
    let album_artist = song
        .album_artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| album_metadata.album_artist.clone());
    let mut metadata = Metadata {
        name: Some(song.title),
        sort_album: album_metadata.sort_album.clone(),
        artist,
        album_artist,
        artist_sort: song.artist_sort,
        album_artist_sort: album_metadata.album_artist_sort.clone(),
        album: Some(song.album.unwrap_or_else(|| album_name.to_owned())),
        year: song
            .year
            .or(album_metadata.year.map(u32::from))
            .and_then(|year| u16::try_from(year).ok()),
        track_current: song.track.map(u64::from),
        disc_current: song.disc_number.map(u64::from),
        mbid_album: album_metadata.mbid_album.clone(),
        ..Metadata::default()
    };
    metadata.artists.extend(artists);
    if metadata.artists.is_empty()
        && let Some(artist) = metadata.artist.clone()
    {
        metadata.artists.push(artist);
    }
    metadata.album_artist_keys.extend(album_artists);
    if metadata.album_artist_keys.is_empty()
        && let Some(artist) = metadata.album_artist.clone()
    {
        metadata.album_artist_keys.push(artist);
    }
    add_genres(&mut metadata, song.genre, song.genres);

    Ok(RemoteTrack {
        location: song.id,
        duration_seconds: song.duration.unwrap_or_default(),
        metadata,
    })
}

fn contributor_names(contributors: Vec<Contributor>) -> Vec<String> {
    let mut names = Vec::new();
    for contributor in contributors {
        let name = contributor.name.trim();
        if !name.is_empty() && !names.iter().any(|current| current == name) {
            names.push(name.to_owned());
        }
    }
    names
}

fn add_genres(metadata: &mut Metadata, genre: Option<String>, genres: Vec<Genre>) {
    for genre in genre
        .into_iter()
        .chain(genres.into_iter().map(|genre| genre.name))
    {
        apply_tag(MetadataTag::Genre(genre), metadata);
    }
}

#[derive(Deserialize)]
struct AlbumListPayload {
    #[serde(rename = "albumList2")]
    album_list: Option<AlbumList>,
}

#[derive(Deserialize)]
struct AlbumPayload {
    album: Option<AlbumResponse>,
}

#[derive(Deserialize)]
struct AlbumList {
    #[serde(default, rename = "album")]
    albums: Vec<AlbumSummary>,
}

#[derive(Deserialize)]
struct AlbumSummary {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumResponse {
    id: String,
    name: String,
    artist: Option<String>,
    sort_name: Option<String>,
    year: Option<u32>,
    music_brainz_id: Option<String>,
    genre: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    album_artists: Vec<Contributor>,
    #[serde(default, rename = "song")]
    songs: Vec<Song>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Song {
    id: String,
    title: String,
    album: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    artist_sort: Option<String>,
    year: Option<u32>,
    track: Option<u32>,
    disc_number: Option<u32>,
    duration: Option<u64>,
    genre: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    artists: Vec<Contributor>,
    #[serde(default)]
    album_artists: Vec<Contributor>,
    #[serde(default)]
    is_dir: bool,
    #[serde(default)]
    is_video: bool,
}

#[derive(Deserialize)]
struct Contributor {
    name: String,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
}
