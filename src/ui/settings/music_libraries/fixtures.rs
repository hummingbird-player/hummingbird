use cntp_i18n::tr;

use super::{
    connection_fields::AuthenticationMode,
    model::{LibraryFixture, LibraryStatus},
};

const FIXTURE_ENV: &str = "HUMMINGBIRD_SUBSONIC_UI_FIXTURE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FixtureKind {
    Empty,
    Connected,
    Importing,
    Offline,
    Error,
    AddPassword,
    AddApiKey,
    Edit,
    EditExpanded,
}

impl FixtureKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "empty" => Some(Self::Empty),
            "connected" => Some(Self::Connected),
            "importing" => Some(Self::Importing),
            "offline" => Some(Self::Offline),
            "error" => Some(Self::Error),
            "add-password" => Some(Self::AddPassword),
            "add-api-key" => Some(Self::AddApiKey),
            "edit" => Some(Self::Edit),
            "edit-expanded" => Some(Self::EditExpanded),
            _ => None,
        }
    }
}

pub(super) enum InitialPage {
    Display,
    Connection(AuthenticationMode),
    Editing { expanded: bool },
}

pub(super) struct FixtureScenario {
    pub(super) libraries: Vec<LibraryFixture>,
    pub(super) initial_page: InitialPage,
}

pub(super) fn load() -> Option<FixtureScenario> {
    let fixture = std::env::var(FIXTURE_ENV)
        .ok()
        .as_deref()
        .and_then(FixtureKind::parse)?;

    let libraries = match fixture {
        FixtureKind::Empty | FixtureKind::AddPassword | FixtureKind::AddApiKey => Vec::new(),
        FixtureKind::Error => vec![error_library()],
        FixtureKind::Importing => vec![LibraryFixture {
            status: LibraryStatus::Importing,
            ..connected_library()
        }],
        FixtureKind::Offline => vec![LibraryFixture {
            status: LibraryStatus::Offline,
            ..connected_library()
        }],
        FixtureKind::Connected | FixtureKind::Edit | FixtureKind::EditExpanded => vec![
            connected_library(),
            LibraryFixture {
                name: "Away library".into(),
                host: "away.example.com".into(),
                username: "william".into(),
                enabled: false,
                status: LibraryStatus::Disabled,
                error: None,
            },
        ],
    };
    let initial_page = match fixture {
        FixtureKind::AddPassword => InitialPage::Connection(AuthenticationMode::Password),
        FixtureKind::AddApiKey => InitialPage::Connection(AuthenticationMode::ApiKey),
        FixtureKind::Edit => InitialPage::Editing { expanded: false },
        FixtureKind::EditExpanded => InitialPage::Editing { expanded: true },
        _ => InitialPage::Display,
    };

    Some(FixtureScenario {
        libraries,
        initial_page,
    })
}

fn connected_library() -> LibraryFixture {
    LibraryFixture {
        name: "Home music".into(),
        host: "music.example.com".into(),
        username: "william".into(),
        enabled: true,
        status: LibraryStatus::Updated,
        error: None,
    }
}

fn error_library() -> LibraryFixture {
    LibraryFixture {
        status: LibraryStatus::SignInRequired,
        error: Some(
            tr!(
                "MUSIC_LIBRARY_FIXTURE_AUTH_ERROR",
                "The server rejected the saved password. Enter a new password and try again."
            )
            .into(),
        ),
        ..connected_library()
    }
}

#[cfg(test)]
mod tests {
    use super::FixtureKind;

    #[test]
    fn fixture_names_are_explicit() {
        assert_eq!(FixtureKind::parse("empty"), Some(FixtureKind::Empty));
        assert_eq!(
            FixtureKind::parse("add-api-key"),
            Some(FixtureKind::AddApiKey)
        );
        assert_eq!(
            FixtureKind::parse("edit-expanded"),
            Some(FixtureKind::EditExpanded)
        );
        assert_eq!(FixtureKind::parse("unknown"), None);
    }
}
