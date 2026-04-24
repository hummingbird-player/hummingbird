use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub name: String,
    pub token: String,
}

#[derive(Deserialize)]
pub struct ValidateToken {
    pub valid: bool,
    pub user_name: Option<String>,
    pub message: Option<String>,
}

#[derive(Serialize)]
pub struct SubmitListens<'a> {
    pub listen_type: &'a str,
    pub payload: Vec<Listen<'a>>,
}

#[derive(Serialize)]
pub struct Listen<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listened_at: Option<i64>,
    pub track_metadata: TrackMetadata<'a>,
}

#[derive(Serialize)]
pub struct TrackMetadata<'a> {
    pub artist_name: &'a str,
    pub track_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_name: Option<&'a str>,
    pub additional_info: AdditionalInfo<'a>,
}

#[derive(Serialize)]
pub struct AdditionalInfo<'a> {
    pub media_player: &'static str,
    pub media_player_version: &'static str,
    pub submission_client: &'static str,
    pub submission_client_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracknumber: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isrc: Option<&'a str>,
}

#[derive(Deserialize)]
pub struct SubmitResponse {
    pub status: String,
}
