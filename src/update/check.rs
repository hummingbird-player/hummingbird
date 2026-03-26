use std::env::var_os;

use chrono::{DateTime, Duration, Utc};
use semver::Version;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::update::{PLATFORM_PACKAGE, ReleaseChannel};

const LATEST_STABLE: &str =
    "https://api.github.com/repos/hummingbird-player/hummingbird/releases/latest";
const UNSTABLE: &str =
    "https://api.github.com/repos/hummingbird-player/hummingbird/releases/191890425";

const ALLOWED_UPLOADERS: &[&str] = &["github-actions[bot]"];

#[derive(Deserialize, Clone, Debug)]
struct Uploader {
    login: String,
}

#[derive(Deserialize, Clone, Debug)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: String,
    updated_at: DateTime<Utc>,
    uploader: Uploader,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug)]
pub(super) struct Update {
    pub url: String,
    pub digest: String,
    pub version: Option<String>,
}

pub async fn check_for_updates(channel: ReleaseChannel) -> anyhow::Result<Option<Update>> {
    let version = env!("CARGO_PKG_VERSION");

    let client = zed_reqwest::Client::builder()
        .user_agent(format!("Hummingbird/{version}"))
        .build()?;

    let release_info: GithubRelease = if channel == ReleaseChannel::Stable {
        client.get(LATEST_STABLE).send().await?
    } else {
        client.get(UNSTABLE).send().await?
    }
    .json()
    .await?;

    let Some(update_available) = (if channel == ReleaseChannel::Stable {
        stable_asset(version, &release_info).await?
    } else {
        unstable_asset(&release_info).await?
    }) else {
        return Ok(None);
    };

    if !ALLOWED_UPLOADERS.contains(&update_available.uploader.login.as_str()) {
        warn!(
            "Update available from disallowed uploader: {}",
            update_available.uploader.login
        );
        warn!(
            "This update will not be downloaded automatically. You may review the update's \
            contents and install it manually if you wish."
        );
        error!(
            "Release '{}' is available but will not be downloaded.",
            release_info.tag_name
        );
        return Ok(None);
    }

    Ok(Some(Update {
        url: update_available.browser_download_url,
        digest: update_available.digest,
        version: if release_info.tag_name == "latest" {
            None
        } else {
            Some(release_info.tag_name)
        },
    }))
}

async fn stable_asset(
    version: &str,
    release_info: &GithubRelease,
) -> anyhow::Result<Option<Asset>> {
    let current_version = Version::parse(version)?;
    let new_version = release_info.tag_name.parse::<Version>()?;

    if new_version > current_version || var_os("HUMMINGBIRD_ALWAYS_UPDATE").is_some() {
        let platform_asset = release_info
            .assets
            .iter()
            .find(|a| a.name == PLATFORM_PACKAGE);
        info!(
            "Found stable asset: {}",
            platform_asset.as_ref().map_or("", |a| &a.name)
        );
        return Ok(platform_asset.cloned());
    }

    Ok(None)
}

async fn unstable_asset(release_info: &GithubRelease) -> anyhow::Result<Option<Asset>> {
    let build_time = DateTime::parse_from_rfc3339(env!("VERGEN_BUILD_TIMESTAMP"))?.to_utc();

    // compare with release info asset
    let asset = release_info
        .assets
        .iter()
        .find(|a| a.name == PLATFORM_PACKAGE);

    let minimum_acceptable_time = build_time + Duration::hours(2);

    if let Some(asset) = asset
        && (asset.updated_at > minimum_acceptable_time
            || var_os("HUMMINGBIRD_ALWAYS_UPDATE").is_some())
    {
        info!("Found unstable asset: {}", asset.name);
        return Ok(Some(asset.clone()));
    }

    Ok(None)
}
