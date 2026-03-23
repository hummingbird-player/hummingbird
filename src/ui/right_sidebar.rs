use gpui::*;
use prelude::FluentBuilder;

use crate::{
    settings::storage::{DEFAULT_LYRICS_FRACTION, DEFAULT_QUEUE_WIDTH},
    ui::{
        components::resizable::{ResizeEdge, resizable},
        lyrics::Lyrics,
        models::Models,
        queue::Queue,
    },
};

// ─── RightSidebar component ───────────────────────────────────────────────────

pub struct RightSidebar {
    queue: Entity<Queue>,
    lyrics: Entity<Lyrics>,
    pub show_queue: Entity<bool>,
    pub show_lyrics: Entity<bool>,
}

impl RightSidebar {
    pub fn new(cx: &mut App, show_queue: Entity<bool>, show_lyrics: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            let queue = Queue::new(cx, show_queue.clone());
            let lyrics = Lyrics::new(cx);

            let queue_width = cx.global::<Models>().queue_width.clone();
            cx.observe(&queue_width, |_, _, cx| cx.notify()).detach();

            let lyrics_height = cx.global::<Models>().lyrics_height.clone();
            cx.observe(&lyrics_height, |_, _, cx| cx.notify()).detach();

            Self {
                queue,
                lyrics,
                show_queue,
                show_lyrics,
            }
        })
    }
}

impl Render for RightSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_queue = *self.show_queue.read(cx);
        let show_lyrics = *self.show_lyrics.read(cx);
        let queue_width = cx.global::<Models>().queue_width.clone();
        let lyrics_height_entity = cx.global::<Models>().lyrics_height.clone();

        let queue = self.queue.clone();
        let lyrics = self.lyrics.clone();

        resizable("queue-resizable", queue_width, ResizeEdge::Left)
            .min_size(px(225.0))
            .max_size(px(800.0))
            .default_size(DEFAULT_QUEUE_WIDTH)
            .h_full()
            .child(
                div()
                    .h_full()
                    .w_full()
                    .flex()
                    .flex_col()
                    // Queue section: fills remaining space above the lyrics pane.
                    .when(show_queue, |outer: Div| {
                        let queue_wrapper = div()
                            .when(show_lyrics, |d: Div| d.flex_1().min_h(px(0.0)))
                            .when(!show_lyrics, |d: Div| d.h_full())
                            .overflow_hidden()
                            .child(queue);
                        outer.child(queue_wrapper)
                    })
                    // Lyrics section: fixed height at bottom, resizable from its top edge.
                    .when(show_lyrics, |outer: Div| {
                        outer
                            .when(show_queue, |outer| {
                                outer.child(
                                    resizable(
                                        "lyrics-resizable",
                                        lyrics_height_entity.clone(),
                                        ResizeEdge::Top,
                                    )
                                    .percent_mode()
                                    .min_size(px(0.10))
                                    .max_size(px(0.85))
                                    .default_size(DEFAULT_LYRICS_FRACTION)
                                    .flex_shrink_0()
                                    .w_full()
                                    .child(div().h_full().overflow_hidden().child(lyrics.clone())),
                                )
                            })
                            .when(!show_queue, |outer| {
                                outer.child(div().h_full().overflow_hidden().child(lyrics))
                            })
                    }),
            )
    }
}
