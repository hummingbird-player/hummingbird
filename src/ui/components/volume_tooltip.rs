use gpui::*;

use crate::ui::{components::tooltip::tooltip_container, theme::Theme};

pub struct VolumeTooltip {
    volume: Entity<f64>,
}

impl VolumeTooltip {
    pub fn new(volume: Entity<f64>, cx: &mut Context<Self>) -> Self {
        cx.observe(&volume, |_, _, cx| {
            cx.notify();
        })
        .detach();

        Self { volume }
    }
}

impl Render for VolumeTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let volume = *self.volume.read(cx);
        let percentage = (volume * 100.0).round() as i32;

        tooltip_container(theme).child(format!("{}%", percentage))
    }
}

pub fn build_volume_tooltip(
    volume: Entity<f64>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_window, cx| cx.new(|cx| VolumeTooltip::new(volume.clone(), cx)).into()
}
