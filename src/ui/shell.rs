use gpui::prelude::FluentBuilder;
use gpui::{AnyElement, App, Div, Entity, IntoElement, ParentElement, Styled, Window, div, px};

use crate::{
    settings::storage::{DEFAULT_QUEUE_WIDTH, DEFAULT_SIDEBAR_WIDTH},
    ui::{
        components::resizable::{ResizeEdge, resizable},
        controls::Controls,
        header::Header,
        layout::{MainRegion, OuterBand, ShellLayout},
        library::{
            Library,
            sidebar::{COLLAPSED_SIDEBAR_WIDTH, Sidebar},
        },
        models::Models,
        right_sidebar::RightSidebar,
        styling::{ActiveTheme, constants::APP_ROUNDING},
    },
};

#[derive(Clone)]
pub(crate) struct Shell {
    pub controls: Entity<Controls>,
    pub right_sidebar: Entity<RightSidebar>,
    pub library_sidebar: Entity<Sidebar>,
    pub library: Entity<Library>,
    pub header: Entity<Header>,
}

impl Shell {
    fn visible_main_regions(&self, layout: &ShellLayout, show_sidebar: bool) -> Vec<MainRegion> {
        layout
            .main_order
            .iter()
            .copied()
            .filter(|region| *region != MainRegion::RightSidebar || show_sidebar)
            .collect()
    }

    fn render_main_region_content(&self, region: MainRegion) -> AnyElement {
        match region {
            MainRegion::LibrarySidebar => self.library_sidebar.clone().into_any_element(),
            MainRegion::LibraryContent => self.library.clone().into_any_element(),
            MainRegion::RightSidebar => self.right_sidebar.clone().into_any_element(),
        }
    }

    fn main_region_resize_edge(
        visible_regions: &[MainRegion],
        index: usize,
        region: MainRegion,
    ) -> Option<ResizeEdge> {
        if !matches!(
            region,
            MainRegion::LibrarySidebar | MainRegion::RightSidebar
        ) {
            return None;
        }

        if index > 0 && visible_regions[index - 1] == MainRegion::LibraryContent {
            return Some(ResizeEdge::Left);
        }

        if index + 1 < visible_regions.len()
            && visible_regions[index + 1] == MainRegion::LibraryContent
        {
            return Some(ResizeEdge::Right);
        }

        None
    }

    fn boundary_has_handle(visible_regions: &[MainRegion], left_index: usize) -> bool {
        let left_region = visible_regions[left_index];
        let right_region = visible_regions[left_index + 1];

        matches!(
            Self::main_region_resize_edge(visible_regions, left_index, left_region),
            Some(ResizeEdge::Right)
        ) || matches!(
            Self::main_region_resize_edge(visible_regions, left_index + 1, right_region),
            Some(ResizeEdge::Left)
        )
    }

    fn render_main_region_slot(
        &self,
        visible_regions: &[MainRegion],
        index: usize,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let region = visible_regions[index];
        let has_left_separator =
            index > 0 && !Self::boundary_has_handle(visible_regions, index - 1);
        let has_right_separator =
            index + 1 < visible_regions.len() && !Self::boundary_has_handle(visible_regions, index);
        let resize_edge = Self::main_region_resize_edge(visible_regions, index, region);

        let content = div()
            .h_full()
            .w_full()
            .overflow_hidden()
            .when(has_left_separator, |div| {
                div.border_l_1().border_color(theme.border_color)
            })
            .when(has_right_separator, |div| {
                div.border_r_1().border_color(theme.border_color)
            })
            .child(self.render_main_region_content(region));

        match region {
            MainRegion::LibraryContent => div()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .child(content)
                .into_any_element(),
            MainRegion::LibrarySidebar => {
                let models = cx.global::<Models>();
                let collapsed = *models.sidebar_collapsed.read(cx);
                let sidebar_width = models.sidebar_width.clone();

                if collapsed {
                    div()
                        .w(COLLAPSED_SIDEBAR_WIDTH)
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                } else if let Some(edge) = resize_edge {
                    resizable("main-sidebar-resizable", sidebar_width.clone(), edge)
                        .min_size(px(175.0))
                        .max_size(px(350.0))
                        .default_size(DEFAULT_SIDEBAR_WIDTH)
                        .h_full()
                        .child(content)
                        .into_any_element()
                } else {
                    div()
                        .w(*sidebar_width.read(cx))
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                }
            }
            MainRegion::RightSidebar => {
                let queue_width = cx.global::<Models>().queue_width.clone();

                if let Some(edge) = resize_edge {
                    resizable("queue-resizable", queue_width.clone(), edge)
                        .min_size(px(225.0))
                        .max_size(px(800.0))
                        .default_size(DEFAULT_QUEUE_WIDTH)
                        .h_full()
                        .child(content)
                        .into_any_element()
                } else {
                    div()
                        .w(*queue_width.read(cx))
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                }
            }
        }
    }

    fn render_main_band(&self, layout: &ShellLayout, show_sidebar: bool, cx: &App) -> Div {
        let visible_regions = self.visible_main_regions(layout, show_sidebar);
        let children = visible_regions
            .iter()
            .enumerate()
            .map(|(index, _)| self.render_main_region_slot(&visible_regions, index, cx))
            .collect::<Vec<_>>();

        div()
            .w_full()
            .h_full()
            .flex()
            .max_w_full()
            .max_h_full()
            .overflow_hidden()
            .children(children)
    }

    fn render_shell_band(
        &self,
        band: OuterBand,
        layout: &ShellLayout,
        show_sidebar: bool,
        cx: &App,
    ) -> AnyElement {
        match band {
            OuterBand::Header => self.header.clone().into_any_element(),
            OuterBand::Main => self
                .render_main_band(layout, show_sidebar, cx)
                .into_any_element(),
            OuterBand::Controls => self.controls.clone().into_any_element(),
        }
    }

    fn render_shell_band_slot(
        &self,
        band: OuterBand,
        layout: &ShellLayout,
        index: usize,
        show_sidebar: bool,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let is_top = index == 0;
        let is_bottom = index + 1 == layout.outer_order.len();
        let theme = cx.theme();
        let decorations = window.window_decorations();

        let slot = div()
            .w_full()
            .overflow_hidden()
            .when(matches!(band, OuterBand::Main), |div| {
                div.flex_1().min_h(px(0.0))
            })
            .when(!is_bottom, |div| {
                div.border_b_1().border_color(theme.border_color)
            })
            .child(self.render_shell_band(band, layout, show_sidebar, cx))
            .map(|div| match decorations {
                gpui::Decorations::Server => div,
                gpui::Decorations::Client { tiling } => div
                    .when(is_top && !(tiling.top || tiling.left), |div| {
                        div.rounded_tl(APP_ROUNDING)
                    })
                    .when(is_top && !(tiling.top || tiling.right), |div| {
                        div.rounded_tr(APP_ROUNDING)
                    })
                    .when(is_bottom && !(tiling.bottom || tiling.left), |div| {
                        div.rounded_bl(APP_ROUNDING)
                    })
                    .when(is_bottom && !(tiling.bottom || tiling.right), |div| {
                        div.rounded_br(APP_ROUNDING)
                    }),
            });

        slot.into_any_element()
    }

    pub fn render_children(
        &self,
        layout: &ShellLayout,
        show_sidebar: bool,
        window: &Window,
        cx: &App,
    ) -> Vec<AnyElement> {
        layout
            .outer_order
            .iter()
            .enumerate()
            .map(|(index, band)| {
                self.render_shell_band_slot(*band, layout, index, show_sidebar, window, cx)
            })
            .rev()
            .collect()
    }
}
