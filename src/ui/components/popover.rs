use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, deferred, div, px, relative,
};
use gpui::{actions, prelude::FluentBuilder};

use crate::ui::theme::Theme;

pub type OnDismissHandler = dyn Fn(&mut Window, &mut App);

actions!(popover, [ClosePopover]);

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Placement relative to the parent bounds; for example, `RightTop` sits to the
/// right of the parent and aligns to its top edge.
pub enum PopoverPosition {
    Left,
    LeftTop,
    LeftBottom,
    Right,
    RightTop,
    RightBottom,
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

#[derive(IntoElement)]
pub struct Popover {
    div: Div,
    position: PopoverPosition,
    edge_offset: Pixels,
    on_dismiss: Option<Rc<OnDismissHandler>>,
}

impl Popover {
    pub fn position(mut self, position: PopoverPosition) -> Self {
        self.position = position;
        self
    }

    pub fn edge_offset(mut self, edge_offset: Pixels) -> Self {
        self.edge_offset = edge_offset;
        self
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl Styled for Popover {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl StatefulInteractiveElement for Popover {}

impl InteractiveElement for Popover {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl RenderOnce for Popover {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let content = self
            .div
            .occlude()
            .bg(theme.elevated_background)
            .border_1()
            .border_color(theme.elevated_border_color)
            .rounded(px(6.0))
            .shadow_md()
            .when_some(self.on_dismiss, |this, on_dismiss| {
                this.on_action(move |_: &ClosePopover, window, cx| {
                    on_dismiss(window, cx);
                })
            });

        deferred(anchor(self.position, self.edge_offset, content))
    }
}

fn anchor(position: PopoverPosition, edge_offset: Pixels, content: Div) -> Div {
    let mut anchor = div().absolute().w(px(0.0)).h(px(0.0));
    let mut content = content.absolute();

    match position {
        PopoverPosition::Left | PopoverPosition::LeftTop | PopoverPosition::LeftBottom => {
            anchor = anchor.left(px(0.0));
            content = content.right(px(0.0)).mr(edge_offset);
        }
        PopoverPosition::Right | PopoverPosition::RightTop | PopoverPosition::RightBottom => {
            anchor = anchor.right(px(0.0));
            content = content.left(px(0.0)).ml(edge_offset);
        }
        PopoverPosition::TopLeft | PopoverPosition::BottomLeft => {
            anchor = anchor.left(px(0.0));
            content = content.left(px(0.0));
        }
        PopoverPosition::TopCenter | PopoverPosition::BottomCenter => {
            anchor = anchor.left(relative(0.5));
            content = content.left(px(0.0)).ml(relative(-0.5));
        }
        PopoverPosition::TopRight | PopoverPosition::BottomRight => {
            anchor = anchor.right(px(0.0));
            content = content.right(px(0.0));
        }
    }

    match position {
        PopoverPosition::TopLeft | PopoverPosition::TopCenter | PopoverPosition::TopRight => {
            anchor = anchor.top(px(0.0));
            content = content.bottom(px(0.0)).mb(edge_offset);
        }
        PopoverPosition::BottomLeft
        | PopoverPosition::BottomCenter
        | PopoverPosition::BottomRight => {
            anchor = anchor.bottom(px(0.0));
            content = content.top(px(0.0)).mt(edge_offset);
        }
        PopoverPosition::LeftTop | PopoverPosition::RightTop => {
            anchor = anchor.top(px(0.0));
            content = content.top(px(0.0));
        }
        PopoverPosition::Left | PopoverPosition::Right => {
            anchor = anchor.top(relative(0.5));
            content = content.top(px(0.0)).mt(relative(-0.5));
        }
        PopoverPosition::LeftBottom | PopoverPosition::RightBottom => {
            anchor = anchor.bottom(px(0.0));
            content = content.bottom(px(0.0));
        }
    }

    anchor.child(content)
}

pub fn popover() -> Popover {
    Popover {
        div: div().p(px(6.0)),
        position: PopoverPosition::BottomCenter,
        edge_offset: px(0.0),
        on_dismiss: None,
    }
}
