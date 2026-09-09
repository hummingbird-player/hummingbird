use cntp_i18n::tr;
use gpui::SharedString;

use super::connection_fields::AuthenticationMode;

pub(super) fn address_error() -> SharedString {
    tr!(
        "MUSIC_LIBRARY_ADDRESS_ERROR",
        "Enter a complete server address, including https://."
    )
    .into()
}

pub(super) fn username_error() -> SharedString {
    tr!("MUSIC_LIBRARY_USERNAME_ERROR", "Enter your username.").into()
}

pub(super) fn secret_error(authentication: AuthenticationMode) -> SharedString {
    if authentication == AuthenticationMode::ApiKey {
        tr!("MUSIC_LIBRARY_API_KEY_ERROR", "Enter your API key.").into()
    } else {
        tr!("MUSIC_LIBRARY_PASSWORD_ERROR", "Enter your password.").into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LibraryStatus {
    Connecting,
    Importing,
    Updated,
    Offline,
    SignInRequired,
    Disabled,
}

impl LibraryStatus {
    pub(super) fn text(self) -> SharedString {
        match self {
            Self::Connecting => tr!("MUSIC_LIBRARY_STATUS_CONNECTING", "Connecting…").into(),
            Self::Importing => tr!(
                "MUSIC_LIBRARY_STATUS_IMPORTING",
                "Importing music… 420 tracks"
            )
            .into(),
            Self::Updated => tr!("MUSIC_LIBRARY_STATUS_UPDATED", "Updated 5 minutes ago").into(),
            Self::Offline => tr!(
                "MUSIC_LIBRARY_STATUS_OFFLINE",
                "Offline — saved music is still available"
            )
            .into(),
            Self::SignInRequired => tr!("MUSIC_LIBRARY_STATUS_SIGN_IN", "Sign-in required").into(),
            Self::Disabled => tr!("MUSIC_LIBRARY_STATUS_DISABLED", "Disabled").into(),
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(not(feature = "libre-services"), allow(dead_code))]
pub(super) struct MusicLibrary {
    pub(super) id: SharedString,
    pub(super) name: SharedString,
    pub(super) address: SharedString,
    pub(super) host: SharedString,
    pub(super) username: SharedString,
    pub(super) authentication: AuthenticationMode,
    pub(super) credential_reference: SharedString,
    pub(super) enabled: bool,
    pub(super) report_playback: bool,
    pub(super) status: LibraryStatus,
    pub(super) error: Option<SharedString>,
}

#[cfg(feature = "libre-services")]
impl MusicLibrary {
    pub(super) fn from_settings(
        settings: &crate::settings::services::MusicLibrarySettings,
    ) -> Option<Self> {
        let host = url::Url::parse(&settings.address)
            .ok()?
            .host_str()?
            .to_owned();
        Some(Self {
            id: settings.id.clone().into(),
            name: settings.name.clone().into(),
            address: settings.address.clone().into(),
            host: host.into(),
            username: settings.username.clone().into(),
            authentication: settings.authentication.into(),
            credential_reference: settings.credential_reference.clone().into(),
            enabled: settings.enabled,
            report_playback: settings.report_playback,
            status: if settings.enabled {
                LibraryStatus::Updated
            } else {
                LibraryStatus::Disabled
            },
            error: None,
        })
    }

    pub(super) fn to_settings(&self) -> crate::settings::services::MusicLibrarySettings {
        crate::settings::services::MusicLibrarySettings {
            id: self.id.to_string(),
            name: self.name.to_string(),
            address: self.address.to_string(),
            username: self.username.to_string(),
            authentication: self.authentication.into(),
            credential_reference: self.credential_reference.to_string(),
            enabled: self.enabled,
            report_playback: self.report_playback,
        }
    }
}

#[cfg(feature = "libre-services")]
impl From<crate::settings::services::MusicLibraryAuthentication> for AuthenticationMode {
    fn from(value: crate::settings::services::MusicLibraryAuthentication) -> Self {
        match value {
            crate::settings::services::MusicLibraryAuthentication::Password => Self::Password,
            crate::settings::services::MusicLibraryAuthentication::ApiKey => Self::ApiKey,
        }
    }
}

#[cfg(feature = "libre-services")]
impl From<AuthenticationMode> for crate::settings::services::MusicLibraryAuthentication {
    fn from(value: AuthenticationMode) -> Self {
        match value {
            AuthenticationMode::Password => Self::Password,
            AuthenticationMode::ApiKey => Self::ApiKey,
        }
    }
}
