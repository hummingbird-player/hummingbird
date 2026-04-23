use std::env::var_os;

use chrono::{DateTime, Duration, Utc};
use semver::Version;
use serde::Deserialize;
use tracing::{error, info, warn};

use crate::settings::update::ReleaseChannel;

const LATEST_STABLE: &str =
    "https://api.github.com/repos/hummingbird-player/hummingbird/releases/latest";
const UNSTABLE: &str =
    "https://api.github.com/repos/hummingbird-player/hummingbird/releases/191890425";

const ALLOWED_UPLOADERS: &[&str] = &["github-actions[bot]"];

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
struct Uploader {
    login: String,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: String,
    updated_at: DateTime<Utc>,
    uploader: Uploader,
}

#[derive(Deserialize, Clone, Debug)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Update {
    pub url: String,
    pub digest: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug)]
struct CurrentBuild<'a> {
    version: &'a str,
    build_time: DateTime<Utc>,
    current_channel: ReleaseChannel,
    target_channel: ReleaseChannel,
    platform_package: &'a str,
    always_update: bool,
}

pub async fn check_for_updates(
    channel: ReleaseChannel,
    package: &str,
) -> anyhow::Result<Option<Update>> {
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

    let build = CurrentBuild {
        version,
        build_time: DateTime::parse_from_rfc3339(env!("VERGEN_BUILD_TIMESTAMP"))?.to_utc(),
        current_channel: current_build_channel(),
        target_channel: channel,
        platform_package: package,
        always_update: var_os("HUMMINGBIRD_ALWAYS_UPDATE").is_some(),
    };

    select_update(&build, &release_info)
}

fn current_build_channel() -> ReleaseChannel {
    match env!("HUMMINGBIRD_CHANNEL") {
        "stable" => ReleaseChannel::Stable,
        _ => ReleaseChannel::Unstable,
    }
}

fn select_update(
    build: &CurrentBuild<'_>,
    release_info: &GithubRelease,
) -> anyhow::Result<Option<Update>> {
    let channel_switched = build.current_channel != build.target_channel;

    let Some(update_available) = (match build.target_channel {
        ReleaseChannel::Stable => select_stable_asset(build, release_info, channel_switched)?,
        ReleaseChannel::Unstable => select_unstable_asset(build, release_info, channel_switched),
    }) else {
        return Ok(None);
    };

    if !is_allowed_uploader(&update_available.uploader.login) {
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
        version: update_version(&release_info.tag_name),
    }))
}

fn select_stable_asset(
    build: &CurrentBuild<'_>,
    release_info: &GithubRelease,
    channel_switched: bool,
) -> anyhow::Result<Option<Asset>> {
    let current_version = Version::parse(build.version)?;
    let new_version = release_info.tag_name.parse::<Version>()?;
    let should_update = channel_switched || build.always_update || new_version > current_version;

    if !should_update {
        return Ok(None);
    }

    let platform_asset = platform_asset(build.platform_package, release_info).cloned();
    info!(
        "Found stable asset: {}",
        platform_asset.as_ref().map_or("", |a| &a.name)
    );

    Ok(platform_asset)
}

fn select_unstable_asset(
    build: &CurrentBuild<'_>,
    release_info: &GithubRelease,
    channel_switched: bool,
) -> Option<Asset> {
    let asset = platform_asset(build.platform_package, release_info).cloned();
    let minimum_acceptable_time = build.build_time + Duration::minutes(30);

    if let Some(asset) = asset
        && (channel_switched || build.always_update || asset.updated_at > minimum_acceptable_time)
    {
        info!("Found unstable asset: {}", asset.name);
        return Some(asset);
    }

    None
}

fn platform_asset<'a>(
    platform_package: &str,
    release_info: &'a GithubRelease,
) -> Option<&'a Asset> {
    release_info
        .assets
        .iter()
        .find(|asset| asset.name == platform_package)
}

fn is_allowed_uploader(login: &str) -> bool {
    ALLOWED_UPLOADERS.contains(&login)
}

fn update_version(tag_name: &str) -> Option<String> {
    (tag_name != "latest").then(|| tag_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PLATFORM_PACKAGE: &str = "hummingbird-x86_64.AppImage";
    const BUILD_TIME: &str = "2026-03-01T00:00:00Z";

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).unwrap().to_utc()
    }

    /// Builds a current-build fixture with version `0.3.0`, build time
    /// `2026-03-01T00:00:00Z`, platform package `hummingbird-x86_64.AppImage`, and
    /// `always_update = false`, letting each test choose the current and target channels.
    fn build(
        current_channel: ReleaseChannel,
        target_channel: ReleaseChannel,
    ) -> CurrentBuild<'static> {
        CurrentBuild {
            version: "0.3.0",
            build_time: at(BUILD_TIME),
            current_channel,
            target_channel,
            platform_package: TEST_PLATFORM_PACKAGE,
            always_update: false,
        }
    }

    /// Builds a GitHub release with the provided `tag_name` and `assets`.
    fn release(tag_name: &str, assets: Vec<Asset>) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.to_string(),
            assets,
        }
    }

    /// Builds a release asset for the given package, update time, and uploader, using
    /// `https://test/{name}` as the download URL and `sha256:{name}` as the digest.
    fn asset(name: &str, updated_at: &str, uploader: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://test/{name}"),
            digest: format!("sha256:{name}"),
            updated_at: at(updated_at),
            uploader: Uploader {
                login: uploader.to_string(),
            },
        }
    }

    /// Finds an update when the stable release version is newer.
    #[test]
    fn newer_stable_version_returns_update() {
        let release = release(
            "0.3.1",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Stable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert_eq!(
            update,
            Some(Update {
                url: format!("https://test/{TEST_PLATFORM_PACKAGE}"),
                digest: format!("sha256:{TEST_PLATFORM_PACKAGE}"),
                version: Some("0.3.1".to_string()),
            })
        );
    }

    /// Skips updates when the stable release version matches the current version.
    #[test]
    fn same_stable_version_returns_no_update() {
        let release = release(
            "0.3.0",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Stable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Skips stable updates when no asset matches the current platform package.
    #[test]
    fn stable_release_without_matching_platform_asset_returns_no_update() {
        let release = release(
            "0.3.1",
            vec![asset(
                "hummingbird-arm.zip",
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Stable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Rejects stable updates uploaded by an untrusted account.
    #[test]
    fn stable_release_from_disallowed_uploader_returns_no_update() {
        let release = release(
            "0.3.1",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "someone-else",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Stable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Honors the always-update override for stable builds.
    #[test]
    fn always_update_forces_stable_update() {
        let mut build = build(ReleaseChannel::Stable, ReleaseChannel::Stable);
        build.always_update = true;

        let release = release(
            "0.3.0",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(&build, &release).unwrap();

        assert_eq!(
            update.as_ref().and_then(|update| update.version.as_deref()),
            Some("0.3.0")
        );
    }

    /// Honors the always-update override for unstable builds.
    #[test]
    fn always_update_forces_unstable_update() {
        let mut build = build(ReleaseChannel::Unstable, ReleaseChannel::Unstable);
        build.always_update = true;

        let release = release(
            "latest",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-01T00:30:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(&build, &release).unwrap();

        assert!(update.is_some());
    }

    /// Finds an unstable update when the asset is newer than the build-time threshold.
    #[test]
    fn unstable_asset_newer_than_threshold_returns_update() {
        let release = release(
            "latest",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-01T02:00:01Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Unstable),
            &release,
        )
        .unwrap();

        assert_eq!(
            update.as_ref().and_then(|update| update.version.as_deref()),
            None
        );
    }

    /// Skips unstable updates when the asset is too close to the current build time.
    #[test]
    fn unstable_asset_inside_threshold_returns_no_update() {
        let release = release(
            "latest",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-01T00:29:59Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Unstable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Skips unstable updates when no asset matches the current platform package.
    #[test]
    fn unstable_release_without_matching_platform_asset_returns_no_update() {
        let release = release(
            "latest",
            vec![asset(
                "other-package.zip",
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Unstable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Rejects unstable updates uploaded by an untrusted account.
    #[test]
    fn unstable_release_from_disallowed_uploader_returns_no_update() {
        let release = release(
            "latest",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "secret-third-uploader",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Unstable),
            &release,
        )
        .unwrap();

        assert_eq!(update, None);
    }

    /// Allows switching from unstable to the stable channel even when versions match.
    #[test]
    fn switching_from_unstable_to_stable_offers_same_version_stable_release() {
        let release = release(
            "0.3.0",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert!(update.is_some());
    }

    /// Allows switching from unstable back to an older stable release.
    #[test]
    fn switching_from_unstable_to_stable_offers_older_stable_release() {
        let release = release(
            "0.2.9",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-02T00:00:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Unstable, ReleaseChannel::Stable),
            &release,
        )
        .unwrap();

        assert!(update.is_some());
    }

    /// Allows switching from stable to unstable even when the unstable asset is not new enough
    /// to pass the normal build-time freshness check.
    #[test]
    fn switching_from_stable_to_unstable_offers_asset_that_would_fail_normal_freshness_check() {
        let release = release(
            "latest",
            vec![asset(
                TEST_PLATFORM_PACKAGE,
                "2026-03-01T00:30:00Z",
                "github-actions[bot]",
            )],
        );

        let update = select_update(
            &build(ReleaseChannel::Stable, ReleaseChannel::Unstable),
            &release,
        )
        .unwrap();

        assert!(update.is_some());
    }
}
