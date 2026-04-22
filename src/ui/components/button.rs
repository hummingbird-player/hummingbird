use gpui::*;

use crate::ui::theme::Theme;

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

impl ButtonStyle {
    fn base<T>(&self, dest: T) -> T
    where
        T: Styled,
    {
        let div = dest.cursor_pointer().flex();

        match self {
            ButtonStyle::Regular => div.shadow_md().rounded(px(4.0)).border_1(),
            ButtonStyle::Minimal => div.background_opacity(0.0).rounded(px(4.0)),
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
    fn base<T>(&self, dest: T) -> T
    where
        T: Styled,
    {
        match self {
            ButtonSize::Regular => dest.px(px(10.0)).py(px(3.0)).text_sm().gap(px(8.0)),
            ButtonSize::Large => dest
                .px(px(12.0))
                .pt(px(4.0))
                .pb(px(3.0))
                .text_sm()
                .gap(px(8.0)),
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

        let mut div = style
            .base(size.base(intent.base(self.div.hover(|v| style.hover(intent.hover(v, cx))), cx)));
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

    pub fn on_mouse_down(
        mut self,
        button: gpui::MouseButton,
        fun: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.div = self.div.on_mouse_down(button, fun);
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

        let mut div = style.base(
            size.base(
                intent.base(
                    self.div
                        .hover(|v| style.hover(intent.hover(v, cx)))
                        .active(|v| style.active(size.active(intent.active(v, cx)))),
                    cx,
                ),
            ),
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
