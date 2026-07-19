use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

use crate::ui::{components::section_header::section_header, equalizer::view::EqualizerView};

pub struct EqualizerSettings {
    view: Entity<EqualizerView>,
}

impl EqualizerSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let view = EqualizerView::new(cx);
        cx.new(|_| Self { view })
    }
}

impl Render for EqualizerSettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("EQUALIZER")))
            .child(self.view.clone())
    }
}
