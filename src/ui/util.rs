use std::{path::Path, sync::Arc};

use globwalk::GlobWalkerBuilder;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, ParentElement, Pixels, Render, RenderImage, Stateful, StyleRefinement,
    Styled, Window,
};
use rustc_hash::FxHashMap;
use tracing::debug;

pub fn prune_views<T>(
    views_model: &Entity<FxHashMap<usize, Entity<T>>>,
    render_counter: &Entity<usize>,
    current: usize,
    cx: &mut App,
) -> bool
where
    T: Render,
{
    let last = *render_counter.read(cx);
    let mut to_remove: Vec<usize> = Vec::new();
    let mut did_remove = false;

    // determine whether or not we are at the start of a new render cycle
    if current < last {
        // we are at the start of a new render cycle
        // prune views that are no longer in the bounds (current..last)
        // don't prune the first view so this still works with uniform_list
        for (idx, _) in views_model.read(cx).iter() {
            if (*idx < current || *idx >= (last + 1)) && *idx != 0_usize {
                to_remove.push(*idx);
            }
        }
    }

    for idx in to_remove {
        did_remove = true;
        views_model.update(cx, |m, _| {
            debug!("Removing view at index: {}", idx);
            m.remove(&idx);
        });
    }

    // update the render counter
    render_counter.update(cx, |m, _| {
        *m = current;
    });

    did_remove
}

pub fn create_or_retrieve_view<T>(
    views_model: &Entity<FxHashMap<usize, Entity<T>>>,
    idx: usize,
    creation_fn: impl FnOnce(&mut App) -> Entity<T>,
    cx: &mut App,
) -> Entity<T>
where
    T: Render,
{
    let view = views_model.read(cx).get(&idx).cloned();
    match view {
        Some(view) => view,
        None => {
            let view = creation_fn(cx);
            views_model.update(cx, |m, _| {
                m.insert(idx, view.clone());
            });
            view
        }
    }
}

pub fn drop_image_from_app(cx: &mut App, image: Arc<RenderImage>) {
    cx.defer(move |cx| {
        debug!("attempting image drop");

        for window in cx.windows() {
            let image = image.clone();

            debug!("dropping an image from {:?}", window.window_id());

            window
                .update(cx, move |_, window, _| {
                    window.drop_image(image).expect("couldn't drop image");
                })
                .expect("couldn't get window");
        }
    });
}

pub enum MaybeStateful<T> {
    Stateful(Stateful<T>),
    NotStateful(T),
}

impl<T> Styled for MaybeStateful<T>
where
    T: Styled,
{
    fn style(&mut self) -> &mut StyleRefinement {
        match self {
            MaybeStateful::Stateful(stateful) => stateful.style(),
            MaybeStateful::NotStateful(not_stateful) => not_stateful.style(),
        }
    }
}

impl<T> Element for MaybeStateful<T>
where
    T: Element,
{
    type RequestLayoutState = T::RequestLayoutState;
    type PrepaintState = T::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        match self {
            MaybeStateful::Stateful(stateful) => stateful.id(),
            MaybeStateful::NotStateful(not_stateful) => not_stateful.id(),
        }
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        match self {
            MaybeStateful::Stateful(stateful) => stateful.source_location(),
            MaybeStateful::NotStateful(not_stateful) => not_stateful.source_location(),
        }
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        match self {
            MaybeStateful::Stateful(stateful) => {
                stateful.request_layout(id, inspector_id, window, cx)
            }
            MaybeStateful::NotStateful(not_stateful) => {
                not_stateful.request_layout(id, inspector_id, window, cx)
            }
        }
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> T::PrepaintState {
        match self {
            MaybeStateful::Stateful(stateful) => {
                stateful.prepaint(id, inspector_id, bounds, state, window, cx)
            }
            MaybeStateful::NotStateful(not_stateful) => {
                not_stateful.prepaint(id, inspector_id, bounds, state, window, cx)
            }
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        match self {
            MaybeStateful::Stateful(stateful) => stateful.paint(
                id,
                inspector_id,
                bounds,
                request_layout,
                prepaint,
                window,
                cx,
            ),
            MaybeStateful::NotStateful(not_stateful) => not_stateful.paint(
                id,
                inspector_id,
                bounds,
                request_layout,
                prepaint,
                window,
                cx,
            ),
        }
    }
}

impl<T> IntoElement for MaybeStateful<T>
where
    T: Element,
{
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<T> ParentElement for MaybeStateful<T>
where
    T: ParentElement,
{
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        match self {
            MaybeStateful::Stateful(stateful) => stateful.extend(elements),
            MaybeStateful::NotStateful(not_stateful) => not_stateful.extend(elements),
        }
    }
}

pub fn find_art_file_for_path(path: &Path) -> Option<Arc<Path>> {
    let parent = path.parent()?;

    let mut glob =
        GlobWalkerBuilder::from_patterns(parent, &["{folder,cover,front}.{jpg,jpeg,png}"])
            .case_insensitive(true)
            .max_depth(1)
            .build()
            .expect("Failed to build album art glob")
            .filter_map(|e| e.ok());

    glob.next().map(|e| Arc::from(e.path()))
}
