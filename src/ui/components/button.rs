use gpui::*;

use crate::ui::{
    scale::{TextStyle, active_density, active_typography, apply_text_style, scale_px_by},
    styling::theme::Theme,
};

use super::styling::AdditionalStyleUtil;

#[derive(Clone, Copy)]
pub enum ButtonSize {
    Regular,
    Large,
}

#[derive(Clone, Copy)]
pub enum ButtonIntent {
    Primary,
    Secondary,
    Warning,
    Danger,
}

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    Regular,
    Minimal,
}

struct ButtonMetrics {
    regular_inline_padding: f32,
    regular_block_padding_start: f32,
    regular_block_padding_end: f32,
    large_inline_padding: f32,
    large_block_padding_start: f32,
    large_block_padding_end: f32,
    content_gap: f32,
    text: TextStyle,
    radius: f32,
}

fn button_metrics(cx: &App) -> ButtonMetrics {
    let density = active_density(cx);

    ButtonMetrics {
        regular_inline_padding: scale_px_by(density, 10.0, 1.0),
        regular_block_padding_start: scale_px_by(density, 3.0, 0.75),
        regular_block_padding_end: scale_px_by(density, 3.0, 0.75),
        large_inline_padding: scale_px_by(density, 12.0, 1.5),
        large_block_padding_start: scale_px_by(density, 4.0, 1.0),
        large_block_padding_end: scale_px_by(density, 3.0, 1.0),
        content_gap: scale_px_by(density, 8.0, 1.0),
        text: active_typography(cx).label,
        radius: 4.0,
    }
}

impl ButtonStyle {
    fn base<T>(&self, dest: T, metrics: &ButtonMetrics) -> T
    where
        T: Styled,
    {
        let div = dest.cursor_pointer().flex();

        match self {
            ButtonStyle::Regular => div.shadow_md().rounded(px(metrics.radius)).border_1(),
            ButtonStyle::Minimal => div.background_opacity(0.0).rounded(px(metrics.radius)),
        }
    }

    fn hover<T>(&self, dest: T) -> T
    where
        T: Styled,
    {
        match self {
            ButtonStyle::Regular => dest,
            ButtonStyle::Minimal => dest.background_opacity(0.5),
        }
    }

    fn active<T>(&self, dest: T) -> T
    where
        T: Styled,
    {
        match self {
            ButtonStyle::Regular => dest,
            ButtonStyle::Minimal => dest.background_opacity(0.5),
        }
    }
}

impl ButtonSize {
    fn base<T>(&self, dest: T, metrics: &ButtonMetrics) -> T
    where
        T: Styled,
    {
        match self {
            ButtonSize::Regular => apply_text_style(
                dest.px(px(metrics.regular_inline_padding))
                    .pt(px(metrics.regular_block_padding_start))
                    .pb(px(metrics.regular_block_padding_end))
                    .gap(px(metrics.content_gap)),
                metrics.text,
            ),
            ButtonSize::Large => apply_text_style(
                dest.px(px(metrics.large_inline_padding))
                    .pt(px(metrics.large_block_padding_start))
                    .pb(px(metrics.large_block_padding_end))
                    .gap(px(metrics.content_gap)),
                metrics.text,
            ),
        }
    }

    // i have no idea what this is about but the text size changes when you click on it unless we
    // have this
    fn active<T>(&self, dest: T) -> T
    where
        T: Styled,
    {
        match self {
            ButtonSize::Regular => dest,
            ButtonSize::Large => dest,
        }
    }
}

impl ButtonIntent {
    fn base<T>(&self, dest: T, cx: &mut App) -> T
    where
        T: Styled,
    {
        let theme = cx.global::<Theme>();

        match self {
            ButtonIntent::Primary => dest
                .bg(theme.button_primary)
                .text_color(theme.button_primary_text)
                .border_color(theme.button_primary_border),
            ButtonIntent::Secondary => dest
                .bg(theme.button_secondary)
                .text_color(theme.button_secondary_text)
                .border_color(theme.button_secondary_border),
            ButtonIntent::Warning => dest
                .bg(theme.button_warning)
                .text_color(theme.button_warning_text)
                .border_color(theme.button_warning_border),
            ButtonIntent::Danger => dest
                .bg(theme.button_danger)
                .text_color(theme.button_danger_text)
                .border_color(theme.button_danger_border),
        }
    }
    fn hover<T>(&self, dest: T, cx: &mut App) -> T
    where
        T: Styled,
    {
        let theme = cx.global::<Theme>();

        match self {
            ButtonIntent::Primary => dest
                .bg(theme.button_primary_hover)
                .border_color(theme.button_primary_border_hover),
            ButtonIntent::Secondary => dest
                .bg(theme.button_secondary_hover)
                .border_color(theme.button_secondary_border_hover),
            ButtonIntent::Warning => dest
                .bg(theme.button_warning_hover)
                .border_color(theme.button_warning_border_hover),
            ButtonIntent::Danger => dest
                .bg(theme.button_danger_hover)
                .border_color(theme.button_danger_border_hover),
        }
    }
    fn active<T>(&self, dest: T, cx: &mut App) -> T
    where
        T: Styled,
    {
        let theme = cx.global::<Theme>();

        match self {
            ButtonIntent::Primary => dest
                .bg(theme.button_primary_active)
                .border_color(theme.button_primary_border_active),
            ButtonIntent::Secondary => dest
                .bg(theme.button_secondary_active)
                .border_color(theme.button_secondary_border_active),
            ButtonIntent::Warning => dest
                .bg(theme.button_warning_active)
                .border_color(theme.button_warning_border_active),
            ButtonIntent::Danger => dest
                .bg(theme.button_danger_active)
                .border_color(theme.button_danger_border_active),
        }
    }
}

#[derive(IntoElement)]
pub struct Button {
    pub(self) div: Div,
    pub(self) style: ButtonStyle,
    pub(self) size: ButtonSize,
    pub(self) intent: ButtonIntent,
    pub(self) refinement: StyleRefinement,
}

impl Button {
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn intent(mut self, intent: ButtonIntent) -> Self {
        self.intent = intent;
        self
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn id(self, id: impl Into<ElementId>) -> InteractiveButton {
        InteractiveButton {
            div: self.div.id(id),
            size: self.size,
            style: self.style,
            intent: self.intent,
            refinement: self.refinement,
        }
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.refinement
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.style;
        let size = self.size;
        let intent = self.intent;
        let metrics = button_metrics(cx);

        let mut div = style.base(
            size.base(
                intent.base(self.div.hover(|v| style.hover(intent.hover(v, cx))), cx),
                &metrics,
            ),
            &metrics,
        );
        div.style().refine(&self.refinement);
        div
    }
}

#[derive(IntoElement)]
pub struct InteractiveButton {
    pub(self) div: Stateful<Div>,
    pub(self) style: ButtonStyle,
    pub(self) size: ButtonSize,
    pub(self) intent: ButtonIntent,
    pub(self) refinement: StyleRefinement,
}

impl InteractiveButton {
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn intent(mut self, intent: ButtonIntent) -> Self {
        self.intent = intent;
        self
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn on_click(mut self, fun: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.div = self.div.on_click(fun);
        self
    }

    pub fn tooltip(mut self, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.div = self.div.tooltip(build);
        self
    }
}

impl Styled for InteractiveButton {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.refinement
    }
}

impl ParentElement for InteractiveButton {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl RenderOnce for InteractiveButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.style;
        let size = self.size;
        let intent = self.intent;
        let metrics = button_metrics(cx);

        let mut div = style.base(
            size.base(
                intent.base(
                    self.div
                        .hover(|v| style.hover(intent.hover(v, cx)))
                        .active(|v| style.active(size.active(intent.active(v, cx)))),
                    cx,
                ),
                &metrics,
            ),
            &metrics,
        );

        div.style().refine(&self.refinement);
        div
    }
}

pub fn button() -> Button {
    Button {
        div: div(),
        style: ButtonStyle::Regular,
        size: ButtonSize::Regular,
        intent: ButtonIntent::Secondary,
        refinement: StyleRefinement::default(),
    }
}
