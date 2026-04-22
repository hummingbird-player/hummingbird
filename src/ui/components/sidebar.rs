use gpui::{
    App, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled,
    Window, deferred, div, prelude::FluentBuilder, px,
};

use crate::{
    settings::storage::DEFAULT_SIDEBAR_WIDTH,
    ui::{
        components::icons::icon,
        customization::scale::{active_density, active_typography, apply_text_style, scale_px},
        styling::ActiveTheme,
        util::MaybeStateful,
    },
};

const SIDEBAR_CONTAINER_GAP: f32 = 4.0;
const SIDEBAR_ITEM_PADDING_INLINE: f32 = 9.0;
const SIDEBAR_ITEM_PADDING_BLOCK: f32 = 7.0;
const SIDEBAR_ITEM_GAP: f32 = 6.0;
const SIDEBAR_ITEM_ICON_SIZE: f32 = 18.0;
const SIDEBAR_COLLAPSED_ITEM_SIZE: f32 = 36.0;
const SIDEBAR_COLLAPSED_TOOLTIP_INLINE_PADDING: f32 = 12.0;
const SIDEBAR_COLLAPSED_TOOLTIP_BLOCK_START: f32 = 6.0;
const SIDEBAR_COLLAPSED_TOOLTIP_BLOCK_END: f32 = 5.0;
const SIDEBAR_SEPARATOR_BLOCK_MARGIN: f32 = 4.0;

#[derive(IntoElement)]
pub struct Sidebar {
    div: MaybeStateful<Div>,
    width: Option<Pixels>,
}

impl Sidebar {
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.div = MaybeStateful::Stateful(match self.div {
            MaybeStateful::NotStateful(div) => div.id(id),
            MaybeStateful::Stateful(div) => div,
        });

        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

impl Styled for Sidebar {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Sidebar {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.div.extend(elements);
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let density = active_density(cx);
        let width: Pixels = match self.width {
            Some(w) => w,
            None => DEFAULT_SIDEBAR_WIDTH,
        };

        self.div
            .w(width)
            .flex()
            .gap(px(scale_px(density, SIDEBAR_CONTAINER_GAP)))
            .flex_col()
    }
}

pub fn sidebar() -> Sidebar {
    Sidebar {
        div: MaybeStateful::NotStateful(div()),
        width: None,
    }
}

#[derive(IntoElement)]
pub struct SidebarItem {
    parent_div: Stateful<Div>,
    children_div: Div,
    icon: Option<&'static str>,
    active: bool,
    collapsed: bool,
    label: Option<SharedString>,
    state_id: ElementId,
}

impl SidebarItem {
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn active(mut self) -> Self {
        self.active = true;
        self
    }

    pub fn collapsed(mut self) -> Self {
        self.collapsed = true;
        self
    }

    pub fn collapsed_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for SidebarItem {
    fn style(&mut self) -> &mut StyleRefinement {
        self.parent_div.style()
    }
}
impl ParentElement for SidebarItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.children_div.extend(elements);
    }
}

impl StatefulInteractiveElement for SidebarItem {}

impl InteractiveElement for SidebarItem {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.parent_div.interactivity()
    }
}

impl RenderOnce for SidebarItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.state_id.clone(), cx, |_, _| false);
        let theme = cx.theme();
        let density = active_density(cx);
        let typography = active_typography(cx);

        let item = self
            .parent_div
            .flex()
            .overflow_x_hidden()
            .when(!self.collapsed, |this| this.w_full())
            .when(self.collapsed, |this| {
                this.size(px(scale_px(density, SIDEBAR_COLLAPSED_ITEM_SIZE)))
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
            })
            .bg(theme.background_primary)
            .border_1()
            // you may ask: what is even the point of setting the border color to this?
            // well, for some as of yet unknown reason, leaving this unset OR leaving this set
            // to transparent_black() results in the hover effects not applying properly.
            // why? i don't know, it makes no god damn sense
            //
            // load bearing color
            .border_color(theme.background_primary)
            .when(self.active, |div| {
                div.bg(theme.nav_button_pressed)
                    .border_color(theme.nav_button_pressed_border)
            })
            .rounded(px(4.0))
            .when(!self.collapsed, |this| {
                this.px(px(scale_px(density, SIDEBAR_ITEM_PADDING_INLINE)))
            })
            .py(px(scale_px(density, SIDEBAR_ITEM_PADDING_BLOCK)))
            .gap(px(scale_px(density, SIDEBAR_ITEM_GAP)))
            .font_weight(FontWeight::SEMIBOLD)
            .hover(|this| {
                this.bg(theme.nav_button_hover)
                    .border_color(theme.nav_button_hover_border)
            })
            .active(|this| {
                this.bg(theme.nav_button_active)
                    .border_color(theme.nav_button_active_border)
            })
            .when_none(&self.icon, |this| {
                this.child(
                    div()
                        .size(px(scale_px(density, SIDEBAR_ITEM_ICON_SIZE)))
                        .flex_shrink_0()
                        .min_w(px(scale_px(density, SIDEBAR_ITEM_ICON_SIZE))),
                )
            })
            .when_some(self.icon, |this, used_icon| {
                this.child(
                    icon(used_icon)
                        .size(px(scale_px(density, SIDEBAR_ITEM_ICON_SIZE)))
                        .flex_shrink_0()
                        .min_w(px(scale_px(density, SIDEBAR_ITEM_ICON_SIZE))),
                )
            })
            .when(!self.collapsed, |this| {
                this.child(apply_text_style(
                    self.children_div
                        .flex_shrink()
                        .flex_col()
                        .flex()
                        .text_ellipsis()
                        .overflow_x_hidden()
                        .w_full(),
                    typography.body,
                ))
            });

        if self.collapsed
            && let Some(label_text) = self.label
        {
            let ref_hover = *state.read(cx);

            div()
                .relative()
                .id(self.state_id)
                .child(item)
                .on_hover({
                    let state = state.clone();
                    move |hover, _, cx| {
                        state.write(cx, *hover);
                    }
                })
                .when(ref_hover, |this| {
                    this.child(deferred(
                        div()
                            .absolute()
                            .left_full()
                            .top_0()
                            .ml(px(4.0))
                            .bg(theme.elevated_background)
                            .border_1()
                            .border_color(theme.elevated_border_color)
                            .rounded(px(4.0))
                            .shadow_sm()
                            .px(px(scale_px(
                                density,
                                SIDEBAR_COLLAPSED_TOOLTIP_INLINE_PADDING,
                            )))
                            .pt(px(scale_px(density, SIDEBAR_COLLAPSED_TOOLTIP_BLOCK_START)))
                            .pb(px(scale_px(density, SIDEBAR_COLLAPSED_TOOLTIP_BLOCK_END)))
                            .text_color(theme.text)
                            .whitespace_nowrap()
                            .child(apply_text_style(div().child(label_text), typography.body)),
                    ))
                })
                .into_any_element()
        } else {
            item.into_any_element()
        }
    }
}

pub fn sidebar_item(id: impl Into<ElementId>) -> SidebarItem {
    let element_id = id.into();
    let state_id = (element_id.clone(), "id").into();
    SidebarItem {
        parent_div: div().id(element_id),
        children_div: div(),
        icon: None,
        active: false,
        collapsed: false,
        label: None,
        state_id,
    }
}

#[derive(IntoElement)]
pub struct SidebarSeparator {}

impl RenderOnce for SidebarSeparator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let density = active_density(cx);

        div()
            .w_full()
            .my(px(scale_px(density, SIDEBAR_SEPARATOR_BLOCK_MARGIN)))
            .border_b_1()
            .border_color(theme.border_color)
    }
}

pub fn sidebar_separator() -> SidebarSeparator {
    SidebarSeparator {}
}
