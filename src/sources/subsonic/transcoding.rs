//! Translate owned host capabilities into either the advertised negotiation
//! extension or legacy stream parameters. Never reconstruct opaque server tokens.
use super::{
    client::{SubsonicClient, malformed},
    media::canonical_format,
};
use crate::{media::capabilities::AudioDecodeProfile, sources::backend::*};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct MediaPlan {
    pub endpoint: &'static str,
    pub parameters: Vec<(&'static str, String)>,
    pub format: Option<String>,
    pub original: bool,
    pub offset_ms: u64,
    pub offset_seeking: bool,
    /// Non-secret encoding identity, separate from the opaque decision token.
    representation: Option<String>,
}
impl MediaPlan {
    /// Legacy servers may preserve an original already below the bitrate cap.
    /// Only accept that optimization when its identity, decoder support and
    /// bandwidth are known. A forced decoder fallback must never return it.
    pub fn can_reopen_original(
        &self,
        actual: &str,
        request: &MediaRequest,
        track: &RemoteTrack,
    ) -> bool {
        let QualityPolicy::Transcode { bitrate_kbps, .. } = request.quality else {
            return false;
        };
        self.endpoint == "stream"
            && !self.original
            && !request.force_transcode
            && track
                .original_format
                .as_deref()
                .is_some_and(|format| canonical_format(format) == actual)
            && track
                .original_bitrate_kbps
                .is_some_and(|bitrate| bitrate > 0 && bitrate <= bitrate_kbps)
            && request
                .supported_formats
                .iter()
                .any(|format| canonical_format(format) == actual)
    }
    pub fn cache_revision(
        &self,
        validator: Option<&str>,
        source_revision: Option<&str>,
    ) -> Option<String> {
        if self.original {
            return validator.or(source_revision).map(str::to_owned);
        }
        if validator.is_none() && source_revision.is_none() {
            return None;
        }
        let encoding =
            serde_json::to_vec(&(validator, source_revision, &self.representation)).ok()?;
        Some(format!(
            "transcode-v1:{:032x}",
            xxhash_rust::xxh3::xxh3_128(&encoding)
        ))
    }
}

pub(super) async fn plan(
    client: &SubsonicClient,
    extensions: &BTreeSet<String>,
    request: &MediaRequest,
    track: &RemoteTrack,
) -> BackendResult<MediaPlan> {
    validate(request)?;
    if request.quality == QualityPolicy::Original {
        return original(request, track);
    }
    if extensions.contains("transcoding") && !request.decode_profiles.is_empty() {
        match negotiate(client, request, track).await {
            Ok(plan) => return Ok(plan),
            // An advertised extension can still be unavailable on older reverse
            // proxies. Authentication, permission, malformed and transient errors
            // must remain visible instead of triggering another expensive request.
            Err(error)
                if matches!(
                    error.kind,
                    BackendErrorKind::Unsupported | BackendErrorKind::NotFound
                ) => {}
            Err(error) => return Err(error),
        }
    }
    legacy(extensions, request, track)
}

fn validate(request: &MediaRequest) -> BackendResult<()> {
    if request.force_transcode && request.quality != QualityPolicy::Automatic {
        return Err(malformed());
    }
    let name = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    };
    if request.supported_formats.len() > 256 || request.decode_profiles.len() > 128 {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    if request.supported_formats.iter().any(|v| !name(v))
        || request.decode_profiles.iter().any(|p| {
            !name(&p.container)
                || !name(&p.codec)
                || p.max_channels == 0
                || p.max_channels > 32
                || p.max_sample_rate == 0
                || p.max_sample_rate > 768000
                || p.codec_profiles.len() > 16
                || p.codec_profiles.iter().any(|v| !name(v))
        })
    {
        return Err(malformed());
    }
    if let QualityPolicy::Transcode {
        format,
        bitrate_kbps,
    } = &request.quality
    {
        if !name(format) || *bitrate_kbps == 0 || *bitrate_kbps > 10000 {
            return Err(malformed());
        }
        if !request
            .supported_formats
            .iter()
            .any(|v| v.eq_ignore_ascii_case(format))
        {
            return Err(BackendError::unsupported());
        }
    }
    Ok(())
}

fn original(request: &MediaRequest, track: &RemoteTrack) -> BackendResult<MediaPlan> {
    Ok(MediaPlan {
        endpoint: "stream",
        parameters: vec![("id", request.location.clone()), ("format", "raw".into())],
        format: track
            .original_format
            .as_ref()
            .map(|v| v.to_ascii_lowercase()),
        original: true,
        offset_ms: 0,
        offset_seeking: false,
        representation: None,
    })
}

fn legacy(
    extensions: &BTreeSet<String>,
    request: &MediaRequest,
    track: &RemoteTrack,
) -> BackendResult<MediaPlan> {
    let (format, bitrate) = match &request.quality {
        QualityPolicy::Original => return original(request, track),
        QualityPolicy::Automatic => {
            if !request.force_transcode
                && track.original_format.as_deref().is_some_and(|format| {
                    request
                        .supported_formats
                        .iter()
                        .any(|v| v.eq_ignore_ascii_case(format))
                })
            {
                return original(request, track);
            }
            // Lossless decoding fallback first; this policy has no bandwidth cap.
            let format = ["flac", "opus", "mp3", "aac", "wav"]
                .into_iter()
                .find(|format| {
                    request
                        .supported_formats
                        .iter()
                        .any(|v| v.eq_ignore_ascii_case(format))
                })
                .ok_or_else(BackendError::unsupported)?;
            (
                format.to_owned(),
                if matches!(format, "flac" | "wav") {
                    0
                } else {
                    320
                },
            )
        }
        QualityPolicy::Transcode {
            format,
            bitrate_kbps,
        } => (format.to_ascii_lowercase(), *bitrate_kbps),
    };
    let offset_seeking = extensions.contains("transcodeOffset");
    if request.offset_ms != 0 && !offset_seeking {
        return Err(BackendError::unsupported());
    }
    // Legacy timeOffset is integral seconds. Report the actual origin, leaving
    // the host to discard the fractional remainder without inventing timestamps.
    let offset_ms = request.offset_ms / 1000 * 1000;
    let mut parameters = vec![
        ("id", request.location.clone()),
        ("format", format.clone()),
        ("maxBitRate", bitrate.to_string()),
        ("estimateContentLength", "false".into()),
    ];
    if offset_ms != 0 {
        parameters.push(("timeOffset", (offset_ms / 1000).to_string()));
    }
    Ok(MediaPlan {
        endpoint: "stream",
        parameters,
        format: Some(format.clone()),
        original: false,
        offset_ms,
        offset_seeking,
        representation: Some(
            serde_json::to_string(&("legacy", &format, bitrate)).map_err(|_| malformed())?,
        ),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Decision {
    can_direct_play: bool,
    can_transcode: bool,
    transcode_params: Option<String>,
    source_stream: Option<StreamDetails>,
    transcode_stream: Option<StreamDetails>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamDetails {
    protocol: String,
    container: String,
    codec: String,
    audio_channels: Option<u16>,
    audio_samplerate: Option<u32>,
    audio_bitrate: Option<u32>,
    audio_profile: Option<String>,
}
fn matches_profile(stream: &StreamDetails, profile: &AudioDecodeProfile) -> bool {
    stream.protocol == "http"
        && canonical_format(&stream.container) == canonical_format(&profile.container)
        && stream.codec.eq_ignore_ascii_case(&profile.codec)
        && stream
            .audio_channels
            .is_some_and(|n| n > 0 && n <= profile.max_channels)
        && stream
            .audio_samplerate
            .is_some_and(|n| n > 0 && n <= profile.max_sample_rate)
        && (profile.codec_profiles.is_empty()
            || stream.audio_profile.as_ref().is_some_and(|actual| {
                profile
                    .codec_profiles
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(actual))
            }))
}

fn target_matches(profile: &AudioDecodeProfile, format: &str) -> bool {
    // Codec-specific presets must not silently choose another codec in the same container.
    match format {
        "opus" => profile.container == "ogg" && profile.codec == "opus",
        "ogg" | "oga" | "vorbis" => profile.container == "ogg" && profile.codec == "vorbis",
        "alac" => profile.container == "mp4" && profile.codec == "alac",
        "m4a" | "mp4" => profile.container == "mp4" && profile.codec == "aac",
        other => profile.container == canonical_format(other),
    }
}

async fn negotiate(
    client: &SubsonicClient,
    request: &MediaRequest,
    track: &RemoteTrack,
) -> BackendResult<MediaPlan> {
    let explicit = match &request.quality {
        QualityPolicy::Transcode {
            format,
            bitrate_kbps,
        } => Some((format.to_ascii_lowercase(), bitrate_kbps * 1000)),
        _ => None,
    };
    let mut targets: Vec<_> = request
        .decode_profiles
        .iter()
        .filter(|p| {
            explicit
                .as_ref()
                .is_none_or(|(format, _)| target_matches(p, format))
        })
        .collect();
    // Prefer a lossless fallback under Automatic, and keep the order deterministic.
    targets.sort_by_key(|p| {
        (
            p.codec != "flac",
            p.codec != "alac",
            p.codec != "opus",
            p.codec != "mp3",
            &p.container,
            &p.codec,
            std::cmp::Reverse(p.max_channels),
            std::cmp::Reverse(p.max_sample_rate),
        )
    });
    if targets.is_empty() {
        return Err(BackendError::unsupported());
    }
    let direct: Vec<_> = request.decode_profiles.iter().filter(|_| explicit.is_none() && !request.force_transcode)
        .map(|p| json!({"containers":[p.container], "audioCodecs":[p.codec], "protocols":["http"], "maxAudioChannels":p.max_channels})).collect();
    let transcoding: Vec<_> = targets.iter().map(|p| json!({"container":p.container, "audioCodec":p.codec, "protocol":"http", "maxAudioChannels":p.max_channels})).collect();
    // The wire format has codec-wide limits, while host profiles are per
    // container/provider. Advertise their union and validate the returned concrete
    // combination below, so adding a provider never narrows an existing codec.
    let mut codecs: std::collections::BTreeMap<&str, (u32, Option<BTreeSet<&str>>)> =
        std::collections::BTreeMap::new();
    for p in &request.decode_profiles {
        let profiles = (!p.codec_profiles.is_empty()).then(|| {
            p.codec_profiles
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        });
        codecs
            .entry(&p.codec)
            .and_modify(|(rate, allowed)| {
                *rate = (*rate).max(p.max_sample_rate);
                if let (Some(existing), Some(additional)) = (allowed.as_mut(), profiles.as_ref()) {
                    existing.extend(additional);
                } else {
                    *allowed = None;
                }
            })
            .or_insert((p.max_sample_rate, profiles));
    }
    let codecs = codecs.into_iter().map(|(codec, (rate, profiles))| {
        let mut limits = vec![json!({"name":"audioSamplerate", "comparison":"LessThanEqual", "values":[rate.to_string()], "required":true})];
        if let Some(profiles) = profiles {
            limits.push(json!({"name":"audioProfile", "comparison":"Equals", "values":profiles, "required":true}));
        }
        json!({"type":"AudioCodec", "name":codec, "limitations":limits})
    }).collect::<Vec<_>>();
    let body = json!({"name":"Hummingbird", "platform":std::env::consts::OS,
        "maxAudioBitrate":explicit.as_ref().map_or(0, |(_,rate)| *rate),
        "maxTranscodingAudioBitrate":explicit.as_ref().map_or(0, |(_,rate)| *rate),
        "directPlayProfiles":direct, "transcodingProfiles":transcoding, "codecProfiles":codecs});
    let response = client
        .post_json(
            "getTranscodeDecision",
            &[
                ("mediaId", request.location.clone()),
                ("mediaType", "song".into()),
            ],
            &body,
        )
        .await?;
    let decision: Decision = serde_json::from_value(
        response
            .get("transcodeDecision")
            .ok_or_else(malformed)?
            .clone(),
    )
    .map_err(|_| malformed())?;
    if explicit.is_none() && !request.force_transcode && decision.can_direct_play {
        let stream = decision.source_stream.as_ref().ok_or_else(malformed)?;
        if request
            .decode_profiles
            .iter()
            .any(|p| matches_profile(stream, p))
        {
            return original(request, track);
        }
    }
    if !decision.can_transcode {
        return Err(BackendError::unsupported());
    }
    let stream = decision.transcode_stream.ok_or_else(malformed)?;
    if !targets.iter().any(|p| matches_profile(&stream, p))
        || explicit.as_ref().is_some_and(|(_, cap)| {
            stream
                .audio_bitrate
                .is_none_or(|rate| rate == 0 || rate > *cap)
        })
    {
        return Err(BackendError::unsupported());
    }
    let parameters = decision
        .transcode_params
        .filter(|p| !p.is_empty() && p.len() <= 16384)
        .ok_or_else(malformed)?;
    let offset_ms = request.offset_ms / 1000 * 1000;
    Ok(MediaPlan {
        endpoint: "getTranscodeStream",
        parameters: vec![
            ("mediaId", request.location.clone()),
            ("mediaType", "song".into()),
            ("transcodeParams", parameters),
            ("offset", (offset_ms / 1000).to_string()),
        ],
        format: Some(stream.container.clone()),
        original: false,
        offset_ms,
        offset_seeking: true,
        representation: Some(
            serde_json::to_string(&(
                "negotiated",
                &stream.container,
                &stream.codec,
                stream.audio_channels,
                stream.audio_samplerate,
                stream.audio_bitrate,
                &stream.audio_profile,
            ))
            .map_err(|_| malformed())?,
        ),
    })
}

#[cfg(test)]
mod tests;
