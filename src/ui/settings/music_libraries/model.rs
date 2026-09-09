use cntp_i18n::tr;
use gpui::SharedString;

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
pub(super) struct LibraryFixture {
    pub(super) name: SharedString,
    pub(super) host: SharedString,
    pub(super) username: SharedString,
    pub(super) enabled: bool,
    pub(super) status: LibraryStatus,
    pub(super) error: Option<SharedString>,
}
