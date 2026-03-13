use std::{marker::PhantomData, sync::Arc, time::Duration};

use cntp_i18n::{I18nString, trn};
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, img, list, prelude::FluentBuilder,
    px,
};
use nucleo::{
    Config, Nucleo, Utf32String,
    pattern::{CaseMatching, Normalization},
};
use rustc_hash::FxHashMap;
use tokio::sync::mpsc::channel;
use tracing::{debug, trace};

use crate::ui::{components::input::EnrichedInputAction, theme::Theme};

const MAX_VISIBLE_PER_CATEGORY: usize = 5;

pub trait PaletteItem {
    fn left_content(&self, cx: &mut App) -> Option<FinderItemLeft>;
    fn middle_content(&self, cx: &mut App) -> SharedString;
    fn right_content(&self, cx: &mut App) -> Option<SharedString>;
    fn is_enabled(&self, _cx: &App) -> bool {
        true
    }
    fn category(&self) -> Option<I18nString> {
        None
    }
}

#[derive(Clone)]
pub struct ExtraItem {
    pub left: Option<FinderItemLeft>,
    pub middle: SharedString,
    pub right: Option<SharedString>,
    pub on_accept: Arc<dyn Fn(&mut App) + Send + Sync>,
}

pub type ExtraItemProvider = Arc<dyn Fn(&str) -> Vec<ExtraItem> + Send + Sync>;

#[allow(type_alias_bounds)]
type ViewsModel<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
= Entity<FxHashMap<usize, Entity<FinderItem<T, MatcherFunc, OnAccept>>>>;

pub enum DisplayEntry<T> {
    Header(I18nString),
    Item(Arc<T>),
    ShowMore(I18nString, usize),
}

impl<T> Clone for DisplayEntry<T> {
    fn clone(&self) -> Self {
        match self {
            DisplayEntry::Header(h) => DisplayEntry::Header(h.clone()),
            DisplayEntry::Item(i) => DisplayEntry::Item(i.clone()),
            DisplayEntry::ShowMore(cat, count) => DisplayEntry::ShowMore(cat.clone(), *count),
        }
    }
}

pub struct Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    query: String,
    matcher: Nucleo<Arc<T>>,
    views_model: ViewsModel<T, MatcherFunc, OnAccept>,
    render_counter: Entity<usize>,
    last_match: Vec<Arc<T>>,
    display_list: Vec<DisplayEntry<T>>,
    extra_providers: Vec<ExtraItemProvider>,
    extra_items: Vec<ExtraItem>,
    list_state: ListState,
    current_selection: Entity<usize>,
    expanded_categories: Vec<I18nString>,
    on_accept: Arc<OnAccept>,
    phantom: PhantomData<MatcherFunc>,
}

impl<T, MatcherFunc, OnAccept> Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    pub fn new(
        cx: &mut App,
        items: Vec<Arc<T>>,
        get_item_display: Arc<MatcherFunc>,
        on_accept: Arc<OnAccept>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let config = Config::DEFAULT;

            // make notification channel
            let (sender, mut receiver) = channel(10);
            let notify = Arc::new(move || {
                // if it's full it doesn't really matter, it'll already update
                _ = sender.try_send(());
            });

            let views_model = cx.new(|_| FxHashMap::default());
            let render_counter = cx.new(|_| 0);

            let matcher = Nucleo::new(config, notify.clone(), None, 1);
            let injector = matcher.injector();

            for item in &items {
                let item_clone = item.clone();
                let search_text = (get_item_display)(&item_clone, cx);
                trace!("Injecting item with search text: '{search_text}'");
                injector.push(item_clone, move |_v, dest| {
                    dest[0] = search_text.clone();
                });
            }

            let weak_self = cx.weak_entity();
            cx.spawn(async move |_, cx| {
                loop {
                    // get all the update notifications
                    // incase we got multiple
                    let mut needs_update = false;
                    while receiver.try_recv().is_ok() {
                        needs_update = true;
                    }

                    if needs_update {
                        if let Some(entity) = weak_self.upgrade() {
                            entity.update(cx, |this: &mut Self, cx| {
                                this.tick(10);

                                let matches: Vec<Arc<T>> = this.get_matches();
                                if matches != this.last_match {
                                    this.last_match = matches;
                                    this.regenerate_list_state(cx);
                                    cx.notify();
                                }
                            });
                        } else {
                            return;
                        }
                    }

                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
            })
            .detach();

            // update when the query updates
            cx.subscribe(&cx.entity(), |this, _, ev: &String, cx| {
                this.set_query(ev.clone(), cx);
            })
            .detach();

            // handle keyboard navigation
            let on_accept_clone = on_accept.clone();
            cx.subscribe(
                &cx.entity(),
                move |this, _, ev: &EnrichedInputAction, cx| match ev {
                    EnrichedInputAction::Previous => {
                        let idx = *this.current_selection.read(cx);
                        if let Some(prev_idx) = this.prev_enabled_index(idx, cx) {
                            this.current_selection.update(cx, |sel, cx| {
                                *sel = prev_idx;
                                cx.notify();
                            });
                        }

                        let idx = *this.current_selection.read(cx);
                        this.list_state.scroll_to_reveal_item(idx);
                    }
                    EnrichedInputAction::Next => {
                        let idx = *this.current_selection.read(cx);
                        if let Some(next_idx) = this.next_enabled_index(idx, cx) {
                            this.current_selection.update(cx, |sel, cx| {
                                *sel = next_idx;
                                cx.notify();
                            });
                        }

                        let idx = *this.current_selection.read(cx);
                        this.list_state.scroll_to_reveal_item(idx);
                    }
                    EnrichedInputAction::Accept => {
                        let idx = *this.current_selection.read(cx);
                        if !this.index_is_enabled(idx, cx) {
                            return;
                        }

                        if idx < this.extra_items.len() {
                            if let Some(extra) = this.extra_items.get(idx) {
                                (extra.on_accept)(cx);
                            }
                        } else {
                            let display_idx = idx.saturating_sub(this.extra_items.len());
                            match this.display_list.get(display_idx) {
                                Some(DisplayEntry::Item(item)) => {
                                    on_accept_clone(item, cx);
                                }
                                Some(DisplayEntry::ShowMore(cat, _)) => {
                                    this.expand_category(cat.clone(), cx);
                                }
                                _ => {}
                            }
                        }
                    }
                },
            )
            .detach();

            // handle item list updates
            let get_item_display_for_updates = get_item_display.clone();
            cx.subscribe(&cx.entity(), move |this, _, items: &Vec<Arc<T>>, cx| {
                this.matcher.restart(false);
                let injector = this.matcher.injector();

                for item in items {
                    let item_clone = item.clone();
                    let search_text = (get_item_display_for_updates)(&item_clone, cx);
                    injector.push(item_clone, move |_v, dest| {
                        dest[0] = search_text.clone();
                    });
                }

                cx.notify();
            })
            .detach();

            let current_selection = cx.new(|_| 0);

            Self {
                query: String::new(),
                matcher,
                views_model,
                last_match: Vec::new(),
                display_list: Vec::new(),
                extra_providers: Vec::new(),
                extra_items: Vec::new(),
                render_counter,
                current_selection,
                expanded_categories: Vec::new(),
                list_state: Self::make_list_state(None),
                on_accept,
                phantom: PhantomData,
            }
        })
    }

    pub fn register_extra_provider(&mut self, provider: ExtraItemProvider, cx: &mut Context<Self>) {
        self.extra_providers.push(provider);
        self.recompute_extra_items();
        self.regenerate_list_state(cx);
    }

    fn build_display_list(
        matches: &[Arc<T>],
        expanded_categories: &Vec<I18nString>,
    ) -> Vec<DisplayEntry<T>> {
        let mut category_order: Vec<I18nString> = Vec::new();
        let mut category_counts: Vec<usize> = Vec::new();
        let mut has_uncategorized = false;

        for item in matches {
            if let Some(cat) = item.category() {
                if let Some(pos) = category_order.iter().position(|c| *c == cat) {
                    category_counts[pos] += 1;
                } else {
                    category_order.push(cat);
                    category_counts.push(1);
                }
            } else {
                has_uncategorized = true;
            }
        }

        if category_order.is_empty() {
            return matches
                .iter()
                .map(|item| DisplayEntry::Item(item.clone()))
                .collect();
        }

        let mut display_list = Vec::with_capacity(
            matches.len() + category_order.len() + if has_uncategorized { 0 } else { 0 },
        );

        if has_uncategorized {
            for item in matches {
                if item.category().is_none() {
                    display_list.push(DisplayEntry::Item(item.clone()));
                }
            }
        }

        for (cat_idx, cat) in category_order.iter().enumerate() {
            display_list.push(DisplayEntry::Header(cat.clone()));
            let total = category_counts[cat_idx];
            let is_expanded = expanded_categories.iter().any(|c| c == cat);
            let limit = if is_expanded || total <= MAX_VISIBLE_PER_CATEGORY {
                total
            } else {
                MAX_VISIBLE_PER_CATEGORY
            };

            let mut emitted = 0;
            for item in matches {
                if item.category().as_ref() == Some(cat) {
                    if emitted < limit {
                        display_list.push(DisplayEntry::Item(item.clone()));
                    }
                    emitted += 1;

                    if emitted >= total {
                        break;
                    }
                }
            }

            if !is_expanded && total > MAX_VISIBLE_PER_CATEGORY {
                display_list.push(DisplayEntry::ShowMore(cat.clone(), total));
            }
        }

        display_list
    }

    fn total_items(&self) -> usize {
        self.extra_items.len() + self.display_list.len()
    }

    fn index_is_enabled(&self, idx: usize, cx: &App) -> bool {
        if idx < self.extra_items.len() {
            true
        } else {
            let display_idx = idx.saturating_sub(self.extra_items.len());
            match self.display_list.get(display_idx) {
                Some(DisplayEntry::Header(_)) => false,
                Some(DisplayEntry::Item(item)) => item.is_enabled(cx),
                Some(DisplayEntry::ShowMore(_, _)) => true,
                None => false,
            }
        }
    }

    fn next_enabled_index(&self, current: usize, cx: &App) -> Option<usize> {
        ((current + 1)..self.total_items()).find(|idx| self.index_is_enabled(*idx, cx))
    }

    fn prev_enabled_index(&self, current: usize, cx: &App) -> Option<usize> {
        (0..current)
            .rev()
            .find(|idx| self.index_is_enabled(*idx, cx))
    }

    fn first_enabled_index(&self, cx: &App) -> Option<usize> {
        (0..self.total_items()).find(|idx| self.index_is_enabled(*idx, cx))
    }

    fn recompute_extra_items(&mut self) {
        let mut new_items: Vec<ExtraItem> = Vec::new();
        for provider in &self.extra_providers {
            let mut provided = (provider)(&self.query);
            new_items.append(&mut provided);
        }
        self.extra_items = new_items;
    }

    pub fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        debug!("Setting query: '{}' (previous: '{}')", query, self.query);
        self.query = query.clone();
        self.expanded_categories.clear();

        self.matcher
            .pattern
            .reparse(0, &query, CaseMatching::Smart, Normalization::Smart, false);

        // recompute dynamic extra items based on query
        self.recompute_extra_items();

        // get some matches ready immediately
        self.tick(20);

        let matches = self.get_matches();

        // if there are extras or the items are different regenerate the list state
        if matches != self.last_match || !self.extra_items.is_empty() {
            self.last_match = matches;
            self.regenerate_list_state(cx);
        }

        self.current_selection.update(cx, |sel, cx| {
            *sel = self.first_enabled_index(cx).unwrap_or(0);
            cx.notify();
        });
        self.list_state
            .scroll_to_reveal_item(*self.current_selection.read(cx));

        cx.notify();
    }

    fn tick(&mut self, iterations: u32) {
        self.matcher.tick(iterations as u64);
    }

    fn get_matches(&self) -> Vec<Arc<T>> {
        let snapshot = self.matcher.snapshot();
        let count = snapshot.matched_item_count();
        let limit = 100.min(count);

        snapshot
            .matched_items(..limit)
            .map(|item| item.data.clone())
            .collect()
    }

    pub fn regenerate_list_state(&mut self, cx: &mut Context<Self>) {
        let matches = self.get_matches();
        let curr_scroll = self.list_state.logical_scroll_top();

        self.display_list = Self::build_display_list(&matches, &self.expanded_categories);

        self.views_model = cx.new(|_| FxHashMap::default());
        self.render_counter = cx.new(|_| 0);

        let total = self.display_list.len() + self.extra_items.len();
        self.list_state = Self::make_list_state(Some(total));
        self.list_state.scroll_to(curr_scroll);
    }

    fn expand_category(&mut self, category: I18nString, cx: &mut Context<Self>) {
        if !self.expanded_categories.iter().any(|c| c == &category) {
            self.expanded_categories.push(category);
        }
        self.regenerate_list_state(cx);
        cx.notify();
    }

    fn make_list_state(total_count: Option<usize>) -> ListState {
        match total_count {
            Some(count) => ListState::new(count, ListAlignment::Top, px(300.0)),
            None => ListState::new(0, ListAlignment::Top, px(64.0)),
        }
    }
}

impl<T, MatcherFunc, OnAccept> EventEmitter<String> for Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
}

impl<T, MatcherFunc, OnAccept> EventEmitter<Vec<Arc<T>>> for Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
}

impl<T, MatcherFunc, OnAccept> EventEmitter<EnrichedInputAction>
    for Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
}

impl<T, MatcherFunc, OnAccept> Render for Finder<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::ui::caching::hummingbird_cache;

        let display_list = self.display_list.clone();
        let extra_items = self.extra_items.clone();
        let views_model = self.views_model.clone();
        let render_counter = self.render_counter.clone();
        let current_selection = self.current_selection.clone();
        let weak_finder = cx.weak_entity();

        div()
            .w_full()
            .h_full()
            .image_cache(hummingbird_cache("finder-cache", 50))
            .id("finder")
            .flex()
            .p(px(4.0))
            .child(
                list(self.list_state.clone(), move |idx, _, cx| {
                    let extras_len = extra_items.len();
                    if idx < extras_len {
                        render_extra_item(
                            &extra_items[idx],
                            idx,
                            &views_model,
                            &render_counter,
                            &current_selection,
                            &weak_finder,
                            cx,
                        )
                    } else {
                        let display_idx = idx - extras_len;
                        match display_list.get(display_idx) {
                            Some(DisplayEntry::Header(header)) => render_header(header, cx),
                            Some(DisplayEntry::Item(item)) => render_item(
                                item,
                                idx,
                                &views_model,
                                &render_counter,
                                &current_selection,
                                &weak_finder,
                                cx,
                            ),
                            Some(DisplayEntry::ShowMore(_, count)) => render_show_more(
                                idx,
                                *count,
                                extras_len,
                                &current_selection,
                                &weak_finder,
                                cx,
                            ),
                            None => div().into_any_element(),
                        }
                    }
                })
                .flex()
                .flex_col()
                .gap(px(2.0))
                .w_full()
                .h_full(),
            )
    }
}

fn render_header(header: &I18nString, cx: &mut App) -> AnyElement {
    let theme = cx.global::<Theme>();
    div()
        .w_full()
        .px(px(10.0))
        .py(px(4.0))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.text_secondary)
        .child(SharedString::from(header.to_string()))
        .into_any_element()
}

fn render_extra_item<T, MatcherFunc, OnAccept>(
    extra: &ExtraItem,
    idx: usize,
    views_model: &ViewsModel<T, MatcherFunc, OnAccept>,
    render_counter: &Entity<usize>,
    current_selection: &Entity<usize>,
    weak_finder: &WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
    cx: &mut App,
) -> AnyElement
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    use crate::ui::util::{create_or_retrieve_view, prune_views};

    prune_views(views_model, render_counter, idx, cx);

    let current_selection = current_selection.clone();
    let weak_finder = weak_finder.clone();
    let extra = extra.clone();

    div()
        .w_full()
        .child(create_or_retrieve_view(
            views_model,
            idx,
            move |cx| {
                FinderItem::new_extra(
                    cx,
                    ("finder-extra-item", idx),
                    idx,
                    &current_selection,
                    weak_finder.clone(),
                    extra.clone(),
                )
            },
            cx,
        ))
        .into_any_element()
}

fn render_item<T, MatcherFunc, OnAccept>(
    item: &Arc<T>,
    idx: usize,
    views_model: &ViewsModel<T, MatcherFunc, OnAccept>,
    render_counter: &Entity<usize>,
    current_selection: &Entity<usize>,
    weak_finder: &WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
    cx: &mut App,
) -> AnyElement
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    use crate::ui::util::{create_or_retrieve_view, prune_views};

    prune_views(views_model, render_counter, idx, cx);
    let item: Arc<T> = item.clone();
    let current_selection = current_selection.clone();
    let weak_finder = weak_finder.clone();

    div()
        .w_full()
        .child(create_or_retrieve_view(
            views_model,
            idx,
            move |cx| {
                FinderItem::new(
                    cx,
                    ("finder-item", idx),
                    &item,
                    idx,
                    &current_selection,
                    weak_finder.clone(),
                    item.clone(),
                )
            },
            cx,
        ))
        .into_any_element()
}

fn render_show_more<T, MatcherFunc, OnAccept>(
    idx: usize,
    count: usize,
    extras_len: usize,
    current_selection: &Entity<usize>,
    weak_finder: &WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
    cx: &mut App,
) -> AnyElement
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    let theme = cx.global::<Theme>();
    let remaining = count.saturating_sub(MAX_VISIBLE_PER_CATEGORY);
    let current_sel = *current_selection.read(cx);
    let is_selected = current_sel == idx;
    let weak = weak_finder.clone();

    div()
        .w_full()
        .px(px(10.0))
        .py(px(6.0))
        .text_xs()
        .text_color(theme.text_secondary)
        .cursor_pointer()
        .id(("finder-show-more", idx))
        .border_1()
        .hover(|this| {
            this.bg(theme.palette_item_hover)
                .border_color(theme.palette_item_border_hover)
        })
        .when(is_selected, |this| {
            this.bg(theme.palette_item_hover)
                .border_color(theme.palette_item_border_hover)
        })
        .rounded(px(6.0))
        .on_click(move |_, _, cx| {
            weak.update(cx, |finder, cx| {
                let display_idx = idx.saturating_sub(extras_len);
                if let Some(DisplayEntry::ShowMore(cat, _)) = finder.display_list.get(display_idx) {
                    finder.expand_category(cat.clone(), cx);
                }
            })
            .expect("finder was rendered without existing")
        })
        .child(trn!(
            "PALETTE_SHOW_MORE",
            "Show {{count}} more...",
            "Show {{count}} more...",
            count = remaining
        ))
        .into_any_element()
}

type OnAcceptOverride = Option<Arc<dyn Fn(&mut App) + Send + Sync>>;

pub struct FinderItem<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    id: ElementId,
    left: Option<FinderItemLeft>,
    middle: SharedString,
    right: Option<SharedString>,
    idx: usize,
    current_selection: usize,
    is_enabled: bool,
    weak_parent: WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
    item_data: Option<Arc<T>>,
    on_accept_override: OnAcceptOverride,
}

#[derive(Clone)]
pub enum FinderItemLeft {
    Text(SharedString),
    Icon(SharedString),
    Image(SharedString),
}

impl<T, MatcherFunc, OnAccept> FinderItem<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    pub fn new(
        cx: &mut App,
        id: impl Into<ElementId>,
        item: &Arc<T>,
        idx: usize,
        current_selection: &Entity<usize>,
        weak_parent: WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
        item_data: Arc<T>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(
                current_selection,
                |this: &mut Self, selection_model, cx: &mut Context<Self>| {
                    this.current_selection = *selection_model.read(cx);
                    cx.notify();
                },
            )
            .detach();

            let left = item.left_content(cx);
            let middle = item.middle_content(cx);
            let right = item.right_content(cx);
            let is_enabled = item.is_enabled(cx);

            Self {
                id: id.into(),
                left,
                middle,
                right,
                idx,
                current_selection: *current_selection.read(cx),
                is_enabled,
                weak_parent,
                item_data: Some(item_data),
                on_accept_override: None,
            }
        })
    }

    pub fn new_extra(
        cx: &mut App,
        id: impl Into<ElementId>,
        idx: usize,
        current_selection: &Entity<usize>,
        weak_parent: WeakEntity<Finder<T, MatcherFunc, OnAccept>>,
        extra: ExtraItem,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(
                current_selection,
                |this: &mut Self, selection_model, cx: &mut Context<Self>| {
                    this.current_selection = *selection_model.read(cx);
                    cx.notify();
                },
            )
            .detach();

            Self {
                id: id.into(),
                left: extra.left.clone(),
                middle: extra.middle.clone(),
                right: extra.right.clone(),
                idx,
                current_selection: *current_selection.read(cx),
                is_enabled: true,
                weak_parent,
                item_data: None,
                on_accept_override: Some(extra.on_accept.clone()),
            }
        })
    }
}

impl<T, MatcherFunc, OnAccept> Render for FinderItem<T, MatcherFunc, OnAccept>
where
    T: Send + Sync + PartialEq + PaletteItem + 'static,
    MatcherFunc: Fn(&Arc<T>, &mut App) -> Utf32String + 'static,
    OnAccept: Fn(&Arc<T>, &mut App) + 'static,
{
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let weak_parent = self.weak_parent.clone();
        let item_data = self.item_data.clone();
        let on_accept_override = self.on_accept_override.clone();
        let is_enabled = self.is_enabled;

        div()
            .px(px(10.0))
            .py(px(6.0))
            .flex()
            .flex_row()
            .items_center()
            .when(is_enabled, |this| this.cursor_pointer())
            .when(!is_enabled, |this| this.cursor_default().opacity(0.5))
            .id(self.id.clone())
            .border_1()
            .when(is_enabled, |this| {
                this.hover(|this| {
                    this.bg(theme.palette_item_hover)
                        .border_color(theme.palette_item_border_hover)
                })
                .active(|this| {
                    this.bg(theme.palette_item_active)
                        .border_color(theme.palette_item_border_active)
                })
            })
            .when(self.current_selection == self.idx && is_enabled, |this| {
                this.bg(theme.palette_item_hover)
                    .border_color(theme.palette_item_border_hover)
            })
            .rounded(px(6.0))
            .on_click(cx.listener(move |_, _, _, cx| {
                if !is_enabled {
                    return;
                }

                if let Some(override_fn) = on_accept_override.clone() {
                    override_fn(cx);
                } else if let Some(parent) = weak_parent.upgrade()
                    && let Some(item) = item_data.clone()
                {
                    parent.update(cx, |finder, cx| {
                        (finder.on_accept)(&item, cx);
                    });
                }
            }))
            .when_some(self.left.clone(), |div_outer, left| {
                div_outer.child(match left {
                    FinderItemLeft::Text(text) => div()
                        .child(text)
                        .text_ellipsis()
                        .text_sm()
                        .text_color(theme.text_secondary)
                        .mr(px(4.0)),
                    FinderItemLeft::Icon(icon_name) => {
                        use crate::ui::components::icons::icon;
                        div()
                            .child(icon(icon_name).w(px(16.0)).h(px(16.0)))
                            .mr(px(8.0))
                    }
                    FinderItemLeft::Image(image_path) => div()
                        .rounded(px(2.0))
                        .bg(theme.album_art_background)
                        .shadow_sm()
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex_shrink_0()
                        .mr(px(8.0))
                        .child(img(image_path).w(px(16.0)).h(px(16.0)).rounded(px(2.0))),
                })
            })
            .child(
                div()
                    .flex_shrink()
                    .font_weight(FontWeight::BOLD)
                    .text_sm()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(self.middle.clone()),
            )
            .when_some(self.right.clone(), |div_outer, right| {
                div_outer.child(
                    div()
                        .ml_auto()
                        .pl(px(8.0))
                        .flex_shrink()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_sm()
                        .text_color(theme.text_secondary)
                        .child(right),
                )
            })
    }
}
