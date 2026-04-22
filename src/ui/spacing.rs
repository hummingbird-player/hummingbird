//! Spacing defaults and overrides
//!
//! The values from `.ron` are base values, not final rendered pixels.
//! components still run them through the global interface scale from settings
//!
//! The important boundary is that spacing is configurable at the family level:
//! `chrome`, `controls`, and `sidebar`. More behavior-heavy surfaces like queue
//! and lyrics still keep their own local geometry.

use gpui::App;
use serde::{Deserialize, Serialize};

use crate::ui::ui_config::UiConfigGlobal;

/// Family-level spacing overrides loaded from `.ron`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chrome: Option<ChromeSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controls: Option<ControlsSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<SidebarSpacingConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChromeSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_button_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_button_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_button_radius: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_group_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_group_margin_inline_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_group_margin_block_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_padding_inline_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_padding_block_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_padding_block_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_item_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_macos_drag_spacer: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_button_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_button_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_button_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_button_icon_text_size: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ControlsSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<InfoSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<PlaybackSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrubber: Option<ScrubberSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<SecondaryControlsSpacingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_popover_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_popover_padding_inline: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaygain_popover_padding_block: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InfoSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_margin_inline: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_margin_block_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_margin_block_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art_bottom_inset: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_offset: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub like_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_size: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaybackSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_toggle_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_toggle_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_side_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_center_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_toggle_block_offset: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrubberSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_separator_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_separator_height: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SecondaryControlsSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_top_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_track_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_track_top_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_track_inline_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider_top_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divider_inline_margin: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SidebarSpacingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_padding_inline: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_padding_block: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_icon_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_item_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_tooltip_inline_padding: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_tooltip_block_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapsed_tooltip_block_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator_block_margin: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_toggle_gap: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_toggle_block_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_toggle_block_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_toggle_padding_block_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav_button_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_padding_block: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_padding_inline_start: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_padding_inline_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats_padding_block_start: Option<f32>,
}

/// Resolved spacing values after merging defaults with any `.ron` overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spacing {
    pub chrome: ChromeSpacing,
    pub controls: ControlsSpacing,
    pub sidebar: SidebarSpacing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeSpacing {
    pub nav_button_size: f32,
    pub nav_button_icon_size: f32,
    pub nav_button_radius: f32,
    pub nav_group_gap: f32,
    pub nav_group_margin_inline_end: f32,
    pub nav_group_margin_block_start: f32,
    pub header_height: f32,
    pub header_padding_inline_start: f32,
    pub header_padding_block_start: f32,
    pub header_padding_block_end: f32,
    pub header_item_gap: f32,
    pub header_macos_drag_spacer: f32,
    pub window_button_width: f32,
    pub window_button_height: f32,
    pub window_button_icon_size: f32,
    pub window_button_icon_text_size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlsSpacing {
    pub info: InfoSpacing,
    pub playback: PlaybackSpacing,
    pub scrubber: ScrubberSpacing,
    pub secondary: SecondaryControlsSpacing,
    pub replaygain_popover_gap: f32,
    pub replaygain_popover_padding_inline: f32,
    pub replaygain_popover_padding_block: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InfoSpacing {
    pub outer_margin_inline: f32,
    pub outer_margin_block_start: f32,
    pub outer_margin_block_end: f32,
    pub item_gap: f32,
    pub art_size: f32,
    pub art_bottom_inset: f32,
    pub preview_size: f32,
    pub preview_offset: f32,
    pub like_padding: f32,
    pub icon_size: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackSpacing {
    pub top_margin: f32,
    pub side_toggle_size: f32,
    pub side_toggle_icon_size: f32,
    pub transport_side_width: f32,
    pub transport_center_width: f32,
    pub transport_height: f32,
    pub transport_icon_size: f32,
    pub outer_gap: f32,
    pub side_toggle_block_offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrubberSpacing {
    pub horizontal_padding: f32,
    pub top_margin: f32,
    pub bottom_margin: f32,
    pub track_height: f32,
    pub time_gap: f32,
    pub duration_separator_padding: f32,
    pub duration_separator_height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecondaryControlsSpacing {
    pub horizontal_padding: f32,
    pub bottom_padding: f32,
    pub button_size: f32,
    pub button_icon_size: f32,
    pub button_top_margin: f32,
    pub volume_track_height: f32,
    pub volume_track_top_margin: f32,
    pub volume_track_inline_margin: f32,
    pub divider_height: f32,
    pub divider_top_margin: f32,
    pub divider_inline_margin: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SidebarSpacing {
    pub container_gap: f32,
    pub item_padding_inline: f32,
    pub item_padding_block: f32,
    pub item_gap: f32,
    pub item_icon_size: f32,
    pub collapsed_item_size: f32,
    pub collapsed_tooltip_inline_padding: f32,
    pub collapsed_tooltip_block_start: f32,
    pub collapsed_tooltip_block_end: f32,
    pub separator_block_margin: f32,
    pub search_toggle_gap: f32,
    pub search_toggle_block_start: f32,
    pub search_toggle_block_end: f32,
    pub search_toggle_padding_block_end: f32,
    pub nav_button_size: f32,
    pub section_padding_block: f32,
    pub section_padding_inline_start: f32,
    pub section_padding_inline_end: f32,
    pub stats_padding_block_start: f32,
}

pub fn active_spacing(cx: &App) -> Spacing {
    let config = cx.global::<UiConfigGlobal>().0.clone();
    resolve_spacing(config.spacing.as_ref())
}

pub fn resolve_spacing(config: Option<&SpacingConfig>) -> Spacing {
    Spacing {
        chrome: resolve_chrome_spacing(config.and_then(|config| config.chrome.as_ref())),
        controls: resolve_controls_spacing(config.and_then(|config| config.controls.as_ref())),
        sidebar: resolve_sidebar_spacing(config.and_then(|config| config.sidebar.as_ref())),
    }
}

fn resolve_chrome_spacing(config: Option<&ChromeSpacingConfig>) -> ChromeSpacing {
    let mut spacing = ChromeSpacing::default();

    if let Some(config) = config {
        spacing.nav_button_size =
            merge_spacing_value(spacing.nav_button_size, config.nav_button_size);
        spacing.nav_button_icon_size =
            merge_spacing_value(spacing.nav_button_icon_size, config.nav_button_icon_size);
        spacing.nav_button_radius =
            merge_spacing_value(spacing.nav_button_radius, config.nav_button_radius);
        spacing.nav_group_gap = merge_spacing_value(spacing.nav_group_gap, config.nav_group_gap);
        spacing.nav_group_margin_inline_end = merge_spacing_value(
            spacing.nav_group_margin_inline_end,
            config.nav_group_margin_inline_end,
        );
        spacing.nav_group_margin_block_start = merge_spacing_value(
            spacing.nav_group_margin_block_start,
            config.nav_group_margin_block_start,
        );
        spacing.header_height = merge_spacing_value(spacing.header_height, config.header_height);
        spacing.header_padding_inline_start = merge_spacing_value(
            spacing.header_padding_inline_start,
            config.header_padding_inline_start,
        );
        spacing.header_padding_block_start = merge_spacing_value(
            spacing.header_padding_block_start,
            config.header_padding_block_start,
        );
        spacing.header_padding_block_end = merge_spacing_value(
            spacing.header_padding_block_end,
            config.header_padding_block_end,
        );
        spacing.header_item_gap =
            merge_spacing_value(spacing.header_item_gap, config.header_item_gap);
        spacing.header_macos_drag_spacer = merge_spacing_value(
            spacing.header_macos_drag_spacer,
            config.header_macos_drag_spacer,
        );
        spacing.window_button_width =
            merge_spacing_value(spacing.window_button_width, config.window_button_width);
        spacing.window_button_height =
            merge_spacing_value(spacing.window_button_height, config.window_button_height);
        spacing.window_button_icon_size = merge_spacing_value(
            spacing.window_button_icon_size,
            config.window_button_icon_size,
        );
        spacing.window_button_icon_text_size = merge_spacing_value(
            spacing.window_button_icon_text_size,
            config.window_button_icon_text_size,
        );
    }

    spacing
}

fn resolve_controls_spacing(config: Option<&ControlsSpacingConfig>) -> ControlsSpacing {
    let mut spacing = ControlsSpacing::default();

    if let Some(config) = config {
        spacing.info = resolve_info_spacing(config.info.as_ref());
        spacing.playback = resolve_playback_spacing(config.playback.as_ref());
        spacing.scrubber = resolve_scrubber_spacing(config.scrubber.as_ref());
        spacing.secondary = resolve_secondary_controls_spacing(config.secondary.as_ref());
        spacing.replaygain_popover_gap = merge_spacing_value(
            spacing.replaygain_popover_gap,
            config.replaygain_popover_gap,
        );
        spacing.replaygain_popover_padding_inline = merge_spacing_value(
            spacing.replaygain_popover_padding_inline,
            config.replaygain_popover_padding_inline,
        );
        spacing.replaygain_popover_padding_block = merge_spacing_value(
            spacing.replaygain_popover_padding_block,
            config.replaygain_popover_padding_block,
        );
    }

    spacing
}

fn resolve_info_spacing(config: Option<&InfoSpacingConfig>) -> InfoSpacing {
    let mut spacing = InfoSpacing::default();

    if let Some(config) = config {
        spacing.outer_margin_inline =
            merge_spacing_value(spacing.outer_margin_inline, config.outer_margin_inline);
        spacing.outer_margin_block_start = merge_spacing_value(
            spacing.outer_margin_block_start,
            config.outer_margin_block_start,
        );
        spacing.outer_margin_block_end = merge_spacing_value(
            spacing.outer_margin_block_end,
            config.outer_margin_block_end,
        );
        spacing.item_gap = merge_spacing_value(spacing.item_gap, config.item_gap);
        spacing.art_size = merge_spacing_value(spacing.art_size, config.art_size);
        spacing.art_bottom_inset =
            merge_spacing_value(spacing.art_bottom_inset, config.art_bottom_inset);
        spacing.preview_size = merge_spacing_value(spacing.preview_size, config.preview_size);
        spacing.preview_offset = merge_spacing_value(spacing.preview_offset, config.preview_offset);
        spacing.like_padding = merge_spacing_value(spacing.like_padding, config.like_padding);
        spacing.icon_size = merge_spacing_value(spacing.icon_size, config.icon_size);
    }

    spacing
}

fn resolve_playback_spacing(config: Option<&PlaybackSpacingConfig>) -> PlaybackSpacing {
    let mut spacing = PlaybackSpacing::default();

    if let Some(config) = config {
        spacing.top_margin = merge_spacing_value(spacing.top_margin, config.top_margin);
        spacing.side_toggle_size =
            merge_spacing_value(spacing.side_toggle_size, config.side_toggle_size);
        spacing.side_toggle_icon_size =
            merge_spacing_value(spacing.side_toggle_icon_size, config.side_toggle_icon_size);
        spacing.transport_side_width =
            merge_spacing_value(spacing.transport_side_width, config.transport_side_width);
        spacing.transport_center_width = merge_spacing_value(
            spacing.transport_center_width,
            config.transport_center_width,
        );
        spacing.transport_height =
            merge_spacing_value(spacing.transport_height, config.transport_height);
        spacing.transport_icon_size =
            merge_spacing_value(spacing.transport_icon_size, config.transport_icon_size);
        spacing.outer_gap = merge_spacing_value(spacing.outer_gap, config.outer_gap);
        spacing.side_toggle_block_offset = merge_spacing_value(
            spacing.side_toggle_block_offset,
            config.side_toggle_block_offset,
        );
    }

    spacing
}

fn resolve_scrubber_spacing(config: Option<&ScrubberSpacingConfig>) -> ScrubberSpacing {
    let mut spacing = ScrubberSpacing::default();

    if let Some(config) = config {
        spacing.horizontal_padding =
            merge_spacing_value(spacing.horizontal_padding, config.horizontal_padding);
        spacing.top_margin = merge_spacing_value(spacing.top_margin, config.top_margin);
        spacing.bottom_margin = merge_spacing_value(spacing.bottom_margin, config.bottom_margin);
        spacing.track_height = merge_spacing_value(spacing.track_height, config.track_height);
        spacing.time_gap = merge_spacing_value(spacing.time_gap, config.time_gap);
        spacing.duration_separator_padding = merge_spacing_value(
            spacing.duration_separator_padding,
            config.duration_separator_padding,
        );
        spacing.duration_separator_height = merge_spacing_value(
            spacing.duration_separator_height,
            config.duration_separator_height,
        );
    }

    spacing
}

fn resolve_secondary_controls_spacing(
    config: Option<&SecondaryControlsSpacingConfig>,
) -> SecondaryControlsSpacing {
    let mut spacing = SecondaryControlsSpacing::default();

    if let Some(config) = config {
        spacing.horizontal_padding =
            merge_spacing_value(spacing.horizontal_padding, config.horizontal_padding);
        spacing.bottom_padding = merge_spacing_value(spacing.bottom_padding, config.bottom_padding);
        spacing.button_size = merge_spacing_value(spacing.button_size, config.button_size);
        spacing.button_icon_size =
            merge_spacing_value(spacing.button_icon_size, config.button_icon_size);
        spacing.button_top_margin =
            merge_spacing_value(spacing.button_top_margin, config.button_top_margin);
        spacing.volume_track_height =
            merge_spacing_value(spacing.volume_track_height, config.volume_track_height);
        spacing.volume_track_top_margin = merge_spacing_value(
            spacing.volume_track_top_margin,
            config.volume_track_top_margin,
        );
        spacing.volume_track_inline_margin = merge_spacing_value(
            spacing.volume_track_inline_margin,
            config.volume_track_inline_margin,
        );
        spacing.divider_height = merge_spacing_value(spacing.divider_height, config.divider_height);
        spacing.divider_top_margin =
            merge_spacing_value(spacing.divider_top_margin, config.divider_top_margin);
        spacing.divider_inline_margin =
            merge_spacing_value(spacing.divider_inline_margin, config.divider_inline_margin);
    }

    spacing
}

fn resolve_sidebar_spacing(config: Option<&SidebarSpacingConfig>) -> SidebarSpacing {
    let mut spacing = SidebarSpacing::default();

    if let Some(config) = config {
        spacing.container_gap = merge_spacing_value(spacing.container_gap, config.container_gap);
        spacing.item_padding_inline =
            merge_spacing_value(spacing.item_padding_inline, config.item_padding_inline);
        spacing.item_padding_block =
            merge_spacing_value(spacing.item_padding_block, config.item_padding_block);
        spacing.item_gap = merge_spacing_value(spacing.item_gap, config.item_gap);
        spacing.item_icon_size = merge_spacing_value(spacing.item_icon_size, config.item_icon_size);
        spacing.collapsed_item_size =
            merge_spacing_value(spacing.collapsed_item_size, config.collapsed_item_size);
        spacing.collapsed_tooltip_inline_padding = merge_spacing_value(
            spacing.collapsed_tooltip_inline_padding,
            config.collapsed_tooltip_inline_padding,
        );
        spacing.collapsed_tooltip_block_start = merge_spacing_value(
            spacing.collapsed_tooltip_block_start,
            config.collapsed_tooltip_block_start,
        );
        spacing.collapsed_tooltip_block_end = merge_spacing_value(
            spacing.collapsed_tooltip_block_end,
            config.collapsed_tooltip_block_end,
        );
        spacing.separator_block_margin = merge_spacing_value(
            spacing.separator_block_margin,
            config.separator_block_margin,
        );
        spacing.search_toggle_gap =
            merge_spacing_value(spacing.search_toggle_gap, config.search_toggle_gap);
        spacing.search_toggle_block_start = merge_spacing_value(
            spacing.search_toggle_block_start,
            config.search_toggle_block_start,
        );
        spacing.search_toggle_block_end = merge_spacing_value(
            spacing.search_toggle_block_end,
            config.search_toggle_block_end,
        );
        spacing.search_toggle_padding_block_end = merge_spacing_value(
            spacing.search_toggle_padding_block_end,
            config.search_toggle_padding_block_end,
        );
        spacing.nav_button_size =
            merge_spacing_value(spacing.nav_button_size, config.nav_button_size);
        spacing.section_padding_block =
            merge_spacing_value(spacing.section_padding_block, config.section_padding_block);
        spacing.section_padding_inline_start = merge_spacing_value(
            spacing.section_padding_inline_start,
            config.section_padding_inline_start,
        );
        spacing.section_padding_inline_end = merge_spacing_value(
            spacing.section_padding_inline_end,
            config.section_padding_inline_end,
        );
        spacing.stats_padding_block_start = merge_spacing_value(
            spacing.stats_padding_block_start,
            config.stats_padding_block_start,
        );
    }

    spacing
}

const fn merge_spacing_value(base: f32, override_value: Option<f32>) -> f32 {
    match override_value {
        Some(value) => value,
        None => base,
    }
}

impl Default for ChromeSpacing {
    fn default() -> Self {
        Self {
            nav_button_size: 28.0,
            nav_button_icon_size: 16.0,
            nav_button_radius: 3.0,
            nav_group_gap: 2.0,
            nav_group_margin_inline_end: 6.0,
            nav_group_margin_block_start: 1.0,
            header_height: 37.0,
            header_padding_inline_start: 12.0,
            header_padding_block_start: 7.0,
            header_padding_block_end: 8.0,
            header_item_gap: 8.0,
            header_macos_drag_spacer: 72.0,
            window_button_width: 36.0,
            window_button_height: 37.0,
            window_button_icon_size: 14.0,
            window_button_icon_text_size: 11.0,
        }
    }
}

impl Default for ControlsSpacing {
    fn default() -> Self {
        Self {
            info: InfoSpacing::default(),
            playback: PlaybackSpacing::default(),
            scrubber: ScrubberSpacing::default(),
            secondary: SecondaryControlsSpacing::default(),
            replaygain_popover_gap: 10.0,
            replaygain_popover_padding_inline: 4.0,
            replaygain_popover_padding_block: 8.0,
        }
    }
}

impl Default for InfoSpacing {
    fn default() -> Self {
        Self {
            outer_margin_inline: 12.0,
            outer_margin_block_start: 12.0,
            outer_margin_block_end: 6.0,
            item_gap: 10.0,
            art_size: 36.0,
            art_bottom_inset: 6.0,
            preview_size: 256.0,
            preview_offset: 26.0,
            like_padding: 4.0,
            icon_size: 14.0,
        }
    }
}

impl Default for PlaybackSpacing {
    fn default() -> Self {
        Self {
            top_margin: 5.0,
            side_toggle_size: 28.0,
            side_toggle_icon_size: 14.0,
            transport_side_width: 30.0,
            transport_center_width: 32.0,
            transport_height: 28.0,
            transport_icon_size: 16.0,
            outer_gap: 6.0,
            side_toggle_block_offset: 3.0,
        }
    }
}

impl Default for ScrubberSpacing {
    fn default() -> Self {
        Self {
            horizontal_padding: 13.0,
            top_margin: 6.0,
            bottom_margin: 6.0,
            track_height: 6.0,
            time_gap: 6.0,
            duration_separator_padding: 6.0,
            duration_separator_height: 30.0,
        }
    }
}

impl Default for SecondaryControlsSpacing {
    fn default() -> Self {
        Self {
            horizontal_padding: 18.0,
            bottom_padding: 2.0,
            button_size: 25.0,
            button_icon_size: 14.0,
            button_top_margin: 2.0,
            volume_track_height: 6.0,
            volume_track_top_margin: 11.0,
            volume_track_inline_margin: 4.0,
            divider_height: 24.0,
            divider_top_margin: 3.0,
            divider_inline_margin: 4.0,
        }
    }
}

impl Default for SidebarSpacing {
    fn default() -> Self {
        Self {
            container_gap: 4.0,
            item_padding_inline: 9.0,
            item_padding_block: 7.0,
            item_gap: 6.0,
            item_icon_size: 18.0,
            collapsed_item_size: 36.0,
            collapsed_tooltip_inline_padding: 12.0,
            collapsed_tooltip_block_start: 6.0,
            collapsed_tooltip_block_end: 5.0,
            separator_block_margin: 4.0,
            search_toggle_gap: 4.0,
            search_toggle_block_start: 2.0,
            search_toggle_block_end: 4.0,
            search_toggle_padding_block_end: 10.0,
            nav_button_size: 38.0,
            section_padding_block: 8.0,
            section_padding_inline_start: 7.0,
            section_padding_inline_end: 8.0,
            stats_padding_block_start: 8.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChromeSpacingConfig, ControlsSpacingConfig, InfoSpacingConfig, PlaybackSpacingConfig,
        SecondaryControlsSpacingConfig, SidebarSpacing, SidebarSpacingConfig, Spacing,
        SpacingConfig, resolve_spacing,
    };

    #[test]
    fn spacing_defaults_match_current_ui_bases() {
        let spacing = resolve_spacing(None);

        assert_eq!(spacing, Spacing::default());
        assert_eq!(spacing.chrome.nav_button_size, 28.0);
        assert_eq!(spacing.chrome.nav_group_gap, 2.0);
        assert_eq!(spacing.controls.playback.transport_height, 28.0);
        assert_eq!(spacing.sidebar.nav_button_size, 38.0);
    }

    #[test]
    fn spacing_overrides_merge_by_family() {
        let spacing = resolve_spacing(Some(&SpacingConfig {
            chrome: Some(ChromeSpacingConfig {
                nav_button_size: Some(20.0),
                nav_group_gap: Some(4.0),
                header_item_gap: Some(10.0),
                ..Default::default()
            }),
            controls: Some(ControlsSpacingConfig {
                info: Some(InfoSpacingConfig {
                    art_size: Some(40.0),
                    item_gap: Some(12.0),
                    ..Default::default()
                }),
                playback: Some(PlaybackSpacingConfig {
                    outer_gap: Some(8.0),
                    ..Default::default()
                }),
                secondary: Some(SecondaryControlsSpacingConfig {
                    button_size: Some(27.0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            sidebar: Some(SidebarSpacingConfig {
                item_padding_inline: Some(11.0),
                nav_button_size: Some(42.0),
                ..Default::default()
            }),
        }));

        assert_eq!(spacing.chrome.nav_button_size, 20.0);
        assert_eq!(spacing.chrome.nav_group_gap, 4.0);
        assert_eq!(spacing.chrome.header_item_gap, 10.0);
        assert_eq!(spacing.controls.info.art_size, 40.0);
        assert_eq!(spacing.controls.info.item_gap, 12.0);
        assert_eq!(spacing.controls.playback.outer_gap, 8.0);
        assert_eq!(spacing.controls.secondary.button_size, 27.0);
        assert_eq!(spacing.sidebar.item_padding_inline, 11.0);
        assert_eq!(spacing.sidebar.nav_button_size, 42.0);
        assert_eq!(spacing.sidebar.item_gap, SidebarSpacing::default().item_gap);
    }
}
