//! Wire translation only. Session ownership, eligibility, persistence and retry
//! policy belong to the host MMBS adapter.
use super::client::{SubsonicClient, malformed};
use crate::sources::backend::*;
use std::collections::BTreeSet;

pub(super) fn apply_compatibility(
    server: &str,
    version: &str,
    capabilities: &mut BTreeSet<Capability>,
) {
    // Verified against gonic v0.22.0 ServeScrobble: stats update precedes
    // optSubmission, and GetID reads only one id. Keep the workaround scoped to
    // the verified version; future releases need their own acceptance evidence.
    if server.eq_ignore_ascii_case("gonic") && version.trim_start_matches('v') == "0.22.0" {
        capabilities.remove(&Capability::NowPlaying);
        capabilities.remove(&Capability::ScrobbleBatch);
    }
}

pub(super) fn supports_batch(version: &str) -> bool {
    let mut parts = version.split('.');
    match (
        parts.next().and_then(|v| v.parse::<u32>().ok()),
        parts.next().and_then(|v| v.parse::<u32>().ok()),
    ) {
        (Some(major), Some(minor)) => (major, minor) >= (1, 8),
        _ => false,
    }
}

pub(super) async fn send(
    client: &SubsonicClient,
    capabilities: &BTreeSet<Capability>,
    report: PlaybackReport,
) -> BackendResult<()> {
    let (endpoint, parameters, capability) = match report {
        PlaybackReport::NowPlaying {
            location,
            started_at_ms,
        } => (
            "scrobble",
            listens(
                vec![ListenReport {
                    location,
                    started_at_ms,
                }],
                false,
            )?,
            Capability::NowPlaying,
        ),
        PlaybackReport::Listen {
            location,
            started_at_ms,
        } => (
            "scrobble",
            listens(
                vec![ListenReport {
                    location,
                    started_at_ms,
                }],
                true,
            )?,
            Capability::Scrobble,
        ),
        PlaybackReport::Listens { listens: reports } => (
            "scrobble",
            listens(reports, true)?,
            Capability::ScrobbleBatch,
        ),
        PlaybackReport::State {
            location,
            position_ms,
            state,
            rate,
            ignore_scrobble,
        } => {
            validate_location(&location)?;
            if !rate.is_finite() || rate <= 0.0 || rate > 16.0 || position_ms > i64::MAX as u64 {
                return Err(malformed());
            }
            let state = match state {
                PlaybackReportState::Starting => "starting",
                PlaybackReportState::Playing => "playing",
                PlaybackReportState::Paused => "paused",
                PlaybackReportState::Stopped => "stopped",
            };
            (
                "reportPlayback",
                vec![
                    ("mediaId", location),
                    ("mediaType", "song".into()),
                    ("positionMs", position_ms.to_string()),
                    ("state", state.into()),
                    ("playbackRate", rate.to_string()),
                    ("ignoreScrobble", ignore_scrobble.to_string()),
                ],
                Capability::PlaybackReport,
            )
        }
    };
    if !capabilities.contains(&capability) {
        return Err(BackendError::unsupported());
    }
    client.json(endpoint, &parameters).await?;
    Ok(())
}
fn validate_location(location: &str) -> BackendResult<()> {
    if location.is_empty() || location.len() > 4096 || location.contains('\0') {
        return Err(malformed());
    }
    Ok(())
}
fn listens(
    reports: Vec<ListenReport>,
    submission: bool,
) -> BackendResult<Vec<(&'static str, String)>> {
    if reports.is_empty() || reports.len() > MAX_REPORT_BATCH {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    let mut parameters = Vec::with_capacity(1 + reports.len() * 2);
    parameters.push(("submission", submission.to_string()));
    let mut bytes = 0;
    for report in reports {
        validate_location(&report.location)?;
        if report.started_at_ms < 0 {
            return Err(malformed());
        }
        bytes += report.location.len() * 3 + 64;
        if bytes > 16 * 1024 {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        // Repeated IDs and times retain their corresponding order; no map may
        // collapse repeat listens or merge songs from different sessions.
        parameters.push(("id", report.location));
        parameters.push(("time", report.started_at_ms.to_string()));
    }
    Ok(parameters)
}

#[cfg(test)]
mod tests;
