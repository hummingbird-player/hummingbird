use cntp_i18n::tr;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

use crate::{
    settings::services::{
        MusicLibraryAudioQuality as AudioQuality, MusicLibraryTranscodeFormat as CustomFormat,
    },
    ui::{
        components::{
            checkbox::checkbox,
            dropdown::dropdown,
            icons::{FOLDER_SEARCH, icon},
            label::label,
            labeled_slider::labeled_slider,
            segmented_control::segmented_control,
        },
        theme::Theme,
    },
};

use super::{connection_fields::render_field, editing::MusicLibraryEditor};

#[derive(Clone, Copy)]
enum FolderKind {
    Music,
    Audiobooks,
}

impl MusicLibraryEditor {
    pub(super) fn render_quality(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let quality = self.quality;
        let format = self.format;
        let bitrate = self.bitrate;
        let description = match quality {
            AudioQuality::Original => tr!(
                "MUSIC_LIBRARY_QUALITY_ORIGINAL_DESCRIPTION",
                "Use the original file without server conversion."
            ),
            AudioQuality::Automatic => tr!(
                "MUSIC_LIBRARY_QUALITY_AUTO_DESCRIPTION",
                "Choose a format and bitrate automatically for this connection."
            ),
            AudioQuality::Custom => tr!(
                "MUSIC_LIBRARY_QUALITY_CUSTOM_DESCRIPTION",
                "Always request the selected format and bitrate."
            ),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                label(
                    "music-library-quality",
                    tr!("MUSIC_LIBRARY_QUALITY", "Audio quality"),
                )
                .subtext(description)
                .w_full()
                .child(
                    segmented_control("music-library-quality-options")
                        .fit_content()
                        .selected(quality)
                        .option(
                            AudioQuality::Original,
                            tr!("MUSIC_LIBRARY_QUALITY_ORIGINAL", "Original"),
                        )
                        .option(
                            AudioQuality::Automatic,
                            tr!("MUSIC_LIBRARY_QUALITY_AUTO", "Auto"),
                        )
                        .option(
                            AudioQuality::Custom,
                            tr!("MUSIC_LIBRARY_QUALITY_CUSTOM", "Custom"),
                        )
                        .on_change({
                            let entity = cx.entity().downgrade();
                            move |quality, _, cx| {
                                entity
                                    .update(cx, |this, cx| {
                                        this.quality = *quality;
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        }),
                ),
            )
            .when(quality == AudioQuality::Custom, |this| {
                this.child(
                    label(
                        "music-library-custom-format",
                        tr!("MUSIC_LIBRARY_CUSTOM_FORMAT", "Format"),
                    )
                    .w_full()
                    .child(
                        dropdown("music-library-custom-format-dropdown")
                            .w(px(250.0))
                            .selected(format)
                            .option(CustomFormat::Opus, "Opus")
                            .option(CustomFormat::Mp3, "MP3")
                            .option(CustomFormat::Aac, "AAC")
                            .option(CustomFormat::Flac, "FLAC")
                            .on_change({
                                let entity = cx.entity().downgrade();
                                move |format, _, cx| {
                                    entity
                                        .update(cx, |this, cx| {
                                            this.format = *format;
                                            cx.notify();
                                        })
                                        .ok();
                                }
                            }),
                    ),
                )
                .when(format != CustomFormat::Flac, |this| {
                    this.child(
                        label(
                            "music-library-custom-bitrate",
                            tr!("MUSIC_LIBRARY_CUSTOM_BITRATE", "Bitrate"),
                        )
                        .w_full()
                        .child(
                            labeled_slider("music-library-custom-bitrate-slider")
                                .w(px(250.0))
                                .min(64.0)
                                .max(320.0)
                                .value(bitrate)
                                .format_value(|value| format!("{value:.0} kb/s").into())
                                .on_change({
                                    let entity = cx.entity().downgrade();
                                    move |value, _, cx| {
                                        let choices = [64.0, 96.0, 128.0, 192.0, 256.0, 320.0];
                                        let nearest = choices
                                            .into_iter()
                                            .min_by(|a, b| {
                                                (value - *a).abs().total_cmp(&(value - *b).abs())
                                            })
                                            .unwrap_or(192.0);
                                        entity
                                            .update(cx, |this, cx| {
                                                this.bitrate = nearest;
                                                cx.notify();
                                            })
                                            .ok();
                                    }
                                }),
                        ),
                    )
                })
            })
    }

    pub(super) fn render_advanced(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let refresh_frequency = self.refresh_frequency;
        let storage_limit = self.storage_limit;
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(render_field(
                "music-library-name",
                tr!("MUSIC_LIBRARY_NAME", "Library name"),
                self.name.clone(),
            ))
            .child(
                label(
                    "music-library-refresh",
                    tr!("MUSIC_LIBRARY_REFRESH_FREQUENCY", "Refresh frequency"),
                )
                .child(
                    labeled_slider("music-library-refresh-slider")
                        .w(px(250.0))
                        .min(0.0)
                        .max(2.0)
                        .value(refresh_frequency)
                        .format_value(|value| match value.round() as i32 {
                            0 => tr!("MUSIC_LIBRARY_REFRESH_MANUAL", "Manual").into(),
                            1 => tr!("MUSIC_LIBRARY_REFRESH_HOURLY", "Hourly").into(),
                            _ => tr!("MUSIC_LIBRARY_REFRESH_DAILY", "Daily").into(),
                        })
                        .on_change({
                            let entity = cx.entity().downgrade();
                            move |value, _, cx| {
                                entity
                                    .update(cx, |this, cx| {
                                        this.refresh_frequency = value.round();
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        }),
                ),
            )
            .child(
                label(
                    "music-library-storage",
                    tr!("MUSIC_LIBRARY_STORAGE_LIMIT", "Temporary storage limit"),
                )
                .subtext(tr!(
                    "MUSIC_LIBRARY_STORAGE_DESCRIPTION",
                    "Limits the playback cache on this device, not the music on your server."
                ))
                .child(
                    labeled_slider("music-library-storage-slider")
                        .w(px(250.0))
                        .min(1.0)
                        .max(20.0)
                        .value(storage_limit)
                        .format_value(|value| format!("{value:.0} GB").into())
                        .on_change({
                            let entity = cx.entity().downgrade();
                            move |value, _, cx| {
                                entity
                                    .update(cx, |this, cx| {
                                        this.storage_limit = value.round();
                                        cx.notify();
                                    })
                                    .ok();
                            }
                        }),
                ),
            )
    }

    pub(super) fn render_folders(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let music_enabled = self.music_folder_enabled;
        let audiobooks_enabled = self.audiobooks_folder_enabled;

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                label(
                    "music-library-folders",
                    tr!("MUSIC_LIBRARY_FOLDERS", "Music folders"),
                )
                .subtext(tr!(
                    "MUSIC_LIBRARY_FOLDERS_DESCRIPTION",
                    "Choose which server folders appear in Hummingbird."
                ))
                .w_full(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(self.render_folder_row(
                        FolderKind::Music,
                        tr!("MUSIC_LIBRARY_FOLDER_MUSIC", "Music").into(),
                        music_enabled,
                        true,
                        cx,
                    ))
                    .child(self.render_folder_row(
                        FolderKind::Audiobooks,
                        tr!("MUSIC_LIBRARY_FOLDER_AUDIOBOOKS", "Audiobooks").into(),
                        audiobooks_enabled,
                        false,
                        cx,
                    )),
            )
    }

    fn render_folder_row(
        &self,
        folder: FolderKind,
        name: SharedString,
        checked: bool,
        first: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let id = match folder {
            FolderKind::Music => "music",
            FolderKind::Audiobooks => "audiobooks",
        };
        div()
            .id(SharedString::from(format!("music-library-folder-{id}")))
            .flex()
            .items_center()
            .gap(px(10.0))
            .pl(px(12.0))
            .pr(px(8.0))
            .py(px(8.0))
            .border_1()
            .when(!first, |this| this.border_t_0())
            .when(first, |this| this.rounded_t(px(6.0)))
            .when(!first, |this| this.rounded_b(px(6.0)))
            .border_color(theme.border_color)
            .bg(theme.background_secondary)
            .cursor_pointer()
            .hover(|this| this.bg(theme.list_item_hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                match folder {
                    FolderKind::Music => this.music_folder_enabled = !checked,
                    FolderKind::Audiobooks => this.audiobooks_folder_enabled = !checked,
                }
                cx.notify();
            }))
            .text_sm()
            .child(
                icon(FOLDER_SEARCH)
                    .size(px(16.0))
                    .flex_shrink_0()
                    .text_color(theme.text_secondary),
            )
            .child(
                div()
                    .flex_grow(1.0)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(name),
            )
            .child(checkbox(
                format!("music-library-folder-{id}-check"),
                checked,
            ))
    }
}
