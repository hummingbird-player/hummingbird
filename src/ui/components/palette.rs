mod finder;

pub use finder::{ExtraItem, ExtraItemProvider, FinderItemLeft, PaletteItem};
use tracing::trace;

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, IntoElement, ParentElement,
    Render, Styled, Window, div, prelude::FluentBuilder, px,
};
use nucleo::Utf32String;

use crate::ui::components::{
    input::{EnrichedInputAction, TextInput},
    palette::finder::Finder,
};
use crate::ui::styling::theme::Theme;

pub struct Palette<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    input: Entity<TextInput>,
    handle: FocusHandle,
    finder: Option<Entity<Finder<T, MatcherFunc, OnAccept>>>,
    items: Vec<Arc<T>>,
    extra_providers: Vec<ExtraItemProvider>,
}

impl<T, MatcherFunc, OnAccept> Palette<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    pub fn new(
        cx: &mut App,
        items: Vec<Arc<T>>,
        matcher: MatcherFunc,
        on_accept: OnAccept,
        show: &Entity<bool>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let handle = cx.focus_handle();
            let weak_self = cx.weak_entity();

            let handler = move |action, _: &mut Window, cx: &mut App| {
                _ = weak_self.update(cx, |this: &mut Self, cx| {
                    if let Some(finder) = &this.finder {
                        cx.update_entity(finder, |_, cx| {
                            cx.emit(action);
                        });
                    }
                });
            };

            let input = TextInput::new(cx, handle.clone(), None, None, Some(Box::new(handler)));

            // Connect input changes to finder
            cx.subscribe(&input, move |this: &mut Self, _, ev: &String, cx| {
                if let Some(finder) = &this.finder {
                    cx.update_entity(finder, |_, cx| {
                        cx.emit(ev.clone());
                    });
                }
            })
            .detach();

            // Forward item list updates to finder
            cx.subscribe(
                &cx.entity(),
                move |this: &mut Self, _, items: &Vec<Arc<T>>, cx| {
                    this.items = items.clone();

                    if let Some(finder) = &this.finder {
                        cx.update_entity(finder, |_, cx| {
                            cx.emit(items.clone());
                        });
                    }
                },
            )
            .detach();

            let matcher = Arc::new(matcher);
            let on_accept = Arc::new(on_accept);

            cx.observe(show, move |this, show, cx| {
                if *show.read(cx) {
                    trace!("Creating finder for palette");

                    let finder =
                        Finder::new(cx, this.items.clone(), matcher.clone(), on_accept.clone());

                    let providers = this.extra_providers.clone();

                    finder.update(cx, |finder, cx| {
                        for provider in providers {
                            finder.register_extra_provider(provider, cx);
                        }

                        cx.notify();
                    });

                    this.finder = Some(finder);
                } else {
                    trace!("Destroying finder for palette");
                    this.finder = None;
                }

                cx.notify()
            })
            .detach();

            Palette {
                input,
                handle,
                finder: None,
                items,
                extra_providers: Vec::new(),
            }
        })
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.handle.focus(window, cx);
    }

    pub fn reset(&self, cx: &mut Context<Self>) {
        cx.update_entity(&self.input, |input, cx| {
            input.reset();
            cx.notify();
        });
        if let Some(finder) = &self.finder {
            cx.update_entity(finder, |finder, cx| {
                finder.set_query("".to_string(), cx);
                finder.regenerate_list_state(cx);
                cx.notify();
            });
        }
    }

    pub fn register_extra_provider(&mut self, provider: ExtraItemProvider, cx: &mut Context<Self>) {
        let provider_clone = provider.clone();

        if let Some(finder) = &self.finder {
            cx.update_entity(finder, |finder, cx| {
                finder.register_extra_provider(provider_clone, cx);
                cx.notify();
            });
        }

        self.extra_providers.push(provider);
    }
}

impl<T, MatcherFunc, OnAccept> Render for Palette<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        div()
            .w_full()
            .h_full()
            .overflow_hidden()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .p(px(12.0))
                    .line_height(px(14.0))
                    .h(px(40.0))
                    .text_sm()
                    .border_b(px(1.0))
                    .border_color(theme.elevated_border_color)
                    .child(self.input.clone()),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .h_full()
                    // FIXME: weird layout issue, this is a hack
                    // eventually this should be removed
                    .pb(px(40.0))
                    .when_some(self.finder.clone(), |this, finder| this.child(finder)),
            )
    }
}

impl<T, MatcherFunc, OnAccept> EventEmitter<Vec<Arc<T>>> for Palette<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
}

impl<T, MatcherFunc, OnAccept> EventEmitter<EnrichedInputAction>
    for Palette<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
}
