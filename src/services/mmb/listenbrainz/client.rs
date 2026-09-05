use chrono::{DateTime, Utc};

use crate::playback::session::SessionMetadata;

use super::types::{
    AdditionalInfo, Listen, Session, SubmitListens, SubmitResponse, TrackMetadata, ValidateToken,
};

pub struct ListenBrainzClient {
    client: zed_reqwest::Client,
    endpoint: url::Url,
    token: String,
}

#[async_trait::async_trait]
impl crate::services::mmb::direct::Client for ListenBrainzClient {
    async fn send(
        &mut self,
        listen: &crate::services::mmb::scrobble::Listen,
        submission: bool,
    ) -> anyhow::Result<()> {
        let artist = listen
            .metadata
            .artist
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing artist"))?;
        let track = listen
            .metadata
            .title
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Missing track"))?;
        let duration = listen.duration_ms.map(|ms| ms / 1000);
        if submission {
            let timestamp = DateTime::<Utc>::from_timestamp_millis(listen.started_at_ms)
                .ok_or_else(|| anyhow::anyhow!("Invalid listen timestamp"))?;
            self.scrobble(artist, track, timestamp, &listen.metadata, duration)
                .await
        } else {
            self.now_playing(artist, track, &listen.metadata, duration)
                .await
        }
    }
}

impl ListenBrainzClient {
    pub fn new(token: String) -> Self {
        ListenBrainzClient {
            token,
            endpoint: "https://api.listenbrainz.org".parse().unwrap(),
            client: zed_reqwest::Client::builder()
                .user_agent("HummingbirdMMBS/1.0")
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
        }
    }

    fn auth(&self, req: zed_reqwest::RequestBuilder) -> zed_reqwest::RequestBuilder {
        req.header("Authorization", format!("Token {}", self.token))
    }

    fn endpoint(&self, path: &str) -> anyhow::Result<url::Url> {
        Ok(self.endpoint.join(path)?)
    }

    pub async fn validate_token(&self) -> anyhow::Result<Session> {
        let url = self.endpoint("/1/validate-token")?;
        let ValidateToken {
            valid,
            user_name,
            message,
        } = self
            .auth(self.client.get(url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if valid {
            let Some(name) = user_name else {
                anyhow::bail!("ListenBrainz token validated without a username");
            };
            Ok(Session {
                name,
                token: self.token.clone(),
            })
        } else {
            anyhow::bail!(message.unwrap_or_else(|| "ListenBrainz token is invalid".to_string()))
        }
    }

    pub async fn scrobble(
        &self,
        artist: &str,
        track: &str,
        timestamp: DateTime<Utc>,
        metadata: &SessionMetadata,
        duration: Option<u64>,
    ) -> anyhow::Result<()> {
        self.submit(
            "single",
            Some(timestamp.timestamp()),
            artist,
            track,
            metadata,
            duration,
        )
        .await
    }

    pub async fn now_playing(
        &self,
        artist: &str,
        track: &str,
        metadata: &SessionMetadata,
        duration: Option<u64>,
    ) -> anyhow::Result<()> {
        self.submit("playing_now", None, artist, track, metadata, duration)
            .await
    }

    async fn submit(
        &self,
        listen_type: &str,
        listened_at: Option<i64>,
        artist: &str,
        track: &str,
        metadata: &SessionMetadata,
        duration: Option<u64>,
    ) -> anyhow::Result<()> {
        let url = self.endpoint("/1/submit-listens")?;
        let payload = listen_payload(listened_at, artist, track, metadata, duration);
        let body = SubmitListens {
            listen_type,
            payload: vec![payload],
        };
        let SubmitResponse { status } = self
            .auth(self.client.post(url))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if status == "ok" {
            Ok(())
        } else {
            anyhow::bail!("ListenBrainz returned status {status}")
        }
    }
}

fn listen_payload<'a>(
    listened_at: Option<i64>,
    artist: &'a str,
    track: &'a str,
    metadata: &'a SessionMetadata,
    duration: Option<u64>,
) -> Listen<'a> {
    Listen {
        listened_at,
        track_metadata: TrackMetadata {
            artist_name: artist,
            track_name: track,
            release_name: metadata.album.as_deref(),
            additional_info: AdditionalInfo {
                media_player: "Hummingbird",
                media_player_version: env!("CARGO_PKG_VERSION"),
                submission_client: "Hummingbird",
                submission_client_version: env!("CARGO_PKG_VERSION"),
                duration,
                release_mbid: metadata.album_mbid.as_deref(),
                tracknumber: metadata.track_number.map(|track| track.to_string()),
                isrc: metadata.isrc.as_deref(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn playing_now_payload_omits_listened_at() {
        let metadata = SessionMetadata::default();
        let payload = listen_payload(None, "artist", "track", &metadata, None);
        let json = serde_json::to_value(payload).unwrap();

        assert!(json.get("listened_at").is_none());
        assert_eq!(json["track_metadata"]["artist_name"], "artist");
        assert_eq!(json["track_metadata"]["track_name"], "track");
    }

    #[test]
    fn single_payload_includes_listened_at() {
        let metadata = SessionMetadata::default();
        let payload = listen_payload(Some(123), "artist", "track", &metadata, None);
        let json = serde_json::to_value(payload).unwrap();

        assert_eq!(json["listened_at"], 123);
    }

    #[test]
    fn payload_includes_available_metadata() {
        let metadata = SessionMetadata {
            album: Some("album".to_string()),
            album_mbid: Some("release-id".to_string()),
            track_number: Some(7),
            isrc: Some("ISRC".to_string()),
            ..SessionMetadata::default()
        };
        let payload = listen_payload(Some(123), "artist", "track", &metadata, Some(300));
        let json = serde_json::to_value(payload).unwrap();
        let info: &Value = &json["track_metadata"]["additional_info"];

        assert_eq!(json["track_metadata"]["release_name"], "album");
        assert_eq!(info["duration"], 300);
        assert_eq!(info["release_mbid"], "release-id");
        assert_eq!(info["tracknumber"], "7");
        assert_eq!(info["isrc"], "ISRC");
    }
}
