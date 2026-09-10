use std::sync::LazyLock;

use cntp_i18n::tr;
use gpui::{
    AppContext, Div, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::{
    settings::SettingsGlobal,
    ui::{
        components::{
            icons::{CLOUD, icon},
            tooltip::build_complex_tooltip,
        },
        theme::Theme,
    },
};

#[derive(Clone, Copy)]
enum SourceFixture {
    None,
    Mixed,
    All,
    Streaming,
}

impl SourceFixture {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("mixed") => Self::Mixed,
            Some("all") => Self::All,
            Some("streaming") => Self::Streaming,
            _ => Self::None,
        }
    }
}

static SOURCE_FIXTURE: LazyLock<SourceFixture> = LazyLock::new(|| {
    SourceFixture::parse(
        std::env::var("HUMMINGBIRD_SOURCE_UI_FIXTURE")
            .ok()
            .as_deref(),
    )
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceOrigin {
    pub name: SharedString,
    pub description: Option<SharedString>,
    pub detail: Option<SharedString>,
}

impl SourceOrigin {
    fn fixture_name() -> SharedString {
        tr!("MUSIC_LIBRARY_FIXTURE_NAME", "Home music").into()
    }

    fn remote_library<C: AppContext>(source_id: &str, cx: &C) -> Self {
        cx.read_global(|settings: &SettingsGlobal, app| {
            settings
                .model
                .read(app)
                .services
                .music_libraries
                .iter()
                .find(|library| library.id == source_id)
                .map(|library| {
                    let description = url::Url::parse(&library.address)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_owned))
                        .map(|host| format!("Subsonic · {host}").into());
                    Self {
                        name: library.name.clone().into(),
                        description,
                        detail: Some(
                            tr!("MUSIC_LIBRARY_QUALITY_ORIGINAL_TOOLTIP", "Original quality")
                                .into(),
                        ),
                    }
                })
        })
        .unwrap_or_else(|| Self {
            name: tr!("REMOTE_LIBRARY", "Remote library").into(),
            description: None,
            detail: None,
        })
    }

    fn fixture_library() -> Self {
        Self {
            name: Self::fixture_name(),
            description: Some("Subsonic · music.example.com".into()),
            detail: Some(tr!("MUSIC_LIBRARY_AVAILABLE_OFFLINE", "Available offline").into()),
        }
    }

    fn fixture_stream() -> Self {
        Self {
            name: Self::fixture_name(),
            description: Some(
                tr!(
                    "MUSIC_LIBRARY_FIXTURE_STREAM_FORMAT",
                    "Streaming as Opus · 192 kb/s"
                )
                .into(),
            ),
            detail: Some(tr!("MUSIC_LIBRARY_FIXTURE_ORIGINAL_FORMAT", "Original: FLAC").into()),
        }
    }
}

pub fn source_origin<C: AppContext>(
    cx: &C,
    source_id: Option<&str>,
    fixture_key: usize,
) -> Option<SourceOrigin> {
    let fixture = match *SOURCE_FIXTURE {
        SourceFixture::None => None,
        SourceFixture::Mixed if fixture_key.is_multiple_of(2) => {
            Some(SourceOrigin::fixture_library())
        }
        SourceFixture::Mixed => None,
        SourceFixture::All => Some(SourceOrigin::fixture_library()),
        SourceFixture::Streaming => Some(SourceOrigin::fixture_stream()),
    };

    fixture.or_else(|| {
        source_id
            .filter(|source_id| *source_id != "local")
            .map(|source_id| SourceOrigin::remote_library(source_id, cx))
    })
}

pub fn source_indicator(
    id: impl Into<ElementId>,
    origin: SourceOrigin,
    color: gpui::Rgba,
) -> impl IntoElement {
    let tooltip_origin = origin.clone();

    div()
        .id(id)
        .size(px(16.0))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .child(icon(CLOUD).size(px(16.0)).text_color(color))
        .tooltip(build_complex_tooltip(move |_, cx| {
            let theme = cx.global::<Theme>();
            div()
                .max_w(px(260.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(tooltip_origin.name.clone()),
                )
                .when_some(tooltip_origin.description.clone(), |this, description| {
                    this.child(description)
                })
                .when_some(tooltip_origin.detail.clone(), |this, detail| {
                    this.child(detail)
                })
        }))
}

pub fn source_indicator_slot(
    id: impl Into<ElementId>,
    origin: Option<SourceOrigin>,
    color: gpui::Rgba,
) -> Div {
    let id = id.into();
    div()
        .size(px(16.0))
        .flex()
        .flex_shrink_0()
        .my_auto()
        .items_center()
        .justify_center()
        .when_some(origin, |this, origin| {
            this.child(source_indicator(id, origin, color))
        })
}
