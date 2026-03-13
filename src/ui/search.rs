pub mod model;
pub mod search_item;

use gpui::*;
use model::SearchModel;

use super::{
    components::modal::modal,
    global_actions::Search,
    library::{NavigationHistory, ViewSwitchMessage},
    models::Models,
};

pub struct SearchView {
    show: Entity<bool>,
    search: Entity<SearchModel>,
    view_switcher: Entity<NavigationHistory>,
}

impl SearchView {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let show = cx.new(|_| false);
            let show_clone = show.clone();
            let search = SearchModel::new(cx, &show);

            App::on_action(cx, move |_: &Search, cx| {
                show_clone.update(cx, |m, cx| {
                    *m = true;
                    cx.notify();
                });
            });

            cx.subscribe(
                &search,
                |this: &mut SearchView, _, ev: &ViewSwitchMessage, cx| {
                    this.view_switcher.update(cx, |_, cx| {
                        cx.emit(*ev);
                    });
                    this.reset(cx);
                },
            )
            .detach();

            cx.observe(&show, |_, _, cx| {
                cx.notify();
            })
            .detach();

            SearchView {
                view_switcher: cx.global::<Models>().switcher_model.clone(),
                show,
                search,
            }
        })
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        cx.update_entity(&self.search, |search, cx| {
            search.reset(cx);
            cx.notify();
        });
        self.show.update(cx, |m, cx| {
            *m = false;
            cx.notify();
        })
    }
}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show = self.show.clone();
        let show_read = show.read(cx);
        let weak = cx.weak_entity();

        if *show_read {
            // Focus the search palette instead of our own handle
            cx.update_entity(&self.search, |search, cx| {
                search.focus(window, cx);
            });

            modal()
                .on_exit(move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.reset(cx);
                    })
                    .expect("failed to update search view")
                })
                .child(div().w(px(550.0)).h(px(500.0)).child(self.search.clone()))
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }
}
