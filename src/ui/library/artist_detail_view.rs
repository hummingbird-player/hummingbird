use std::{rc::Rc, sync::Arc};

use cntp_i18n::tr;
use gpui::*;
use prelude::FluentBuilder;
use rustc_hash::FxHashMap;

use crate::{
    library::{
        db::{LikedTrackSortMethod, SortDirection, albums, artists, tracks},
        scan::ScanEvent,
        types::{Album, DBString, Track, table::AlbumColumn},
    },
    media::numbering::NumberDisplayMode,
    playback::{queue::QueueItemData, thread::PlaybackState},
    ui::{
        app::Pool,
        availability::{has_available_tracks, snapshot},
        components::{
            async_resource::AsyncResource,
            button::{ButtonSize, button},
            dropdown::dropdown,
            icons::{SORT_ASCENDING, SORT_DESCENDING, icon},
            playback_controls::playback_controls,
            scrollbar::floating_scrollbar,
            table::{
                grid_item::GridItem,
                table_data::{GridContext, TABLE_MAX_WIDTH},
            },
            tooltip::build_tooltip,
            uniform_grid::uniform_grid,
        },
        constants::REGULAR_BUTTON_ICON_SIZE,
        library::{
            context_menus::AlbumContextMenuContext,
            library_view_header::LibraryViewHeader,
            track_listing::{
                ArtistNameVisibility,
                track_item::{TrackItem, TrackItemLeftField},
            },
        },
        models::{Models, PlaybackInfo, PlaylistEvent},
        theme::Theme,
        util::{create_or_retrieve_view, prune_views},
    },
};

use super::{ViewSwitchMessage, detail_view_padding};

type GridHandler = dyn Fn(&mut App, &u32) + 'static;

pub struct ArtistDetailView {
    artist_id: i64,
    artist_name: Option<DBString>,
    album_ids: Vec<(u32, String)>,
    liked_track_items: Vec<Entity<TrackItem>>,
    standalone_track_items: Vec<Entity<TrackItem>>,
    all_tracks: Arc<Vec<Track>>,
    liked_tracks: Arc<Vec<Track>>,
    standalone_tracks: Arc<Vec<Track>>,
    scroll_handle: ScrollHandle,
    grid_views: Entity<FxHashMap<usize, Entity<GridItem<Album, AlbumColumn>>>>,
    grid_render_counter: Entity<usize>,
    nav_model: Entity<super::NavigationHistory>,
    liked_sort: LikedTrackSortMethod,
    standalone_sort: LikedTrackSortMethod,
    resource:
        Entity<AsyncResource<(i64, LikedTrackSortMethod, LikedTrackSortMethod), ArtistDetailData>>,
    loaded: bool,
}

#[derive(Clone)]
struct ArtistDetailData {
    artist_name: Option<DBString>,
    album_ids: Vec<(u32, String)>,
    all_tracks: Arc<Vec<Track>>,
    liked_tracks: Arc<Vec<Track>>,
    standalone_tracks: Arc<Vec<Track>>,
}

impl ArtistDetailView {
    pub(super) fn new(
        cx: &mut App,
        artist_id: i64,
        nav_model: Entity<super::NavigationHistory>,
    ) -> Entity<Self> {
        let view: Entity<Self> = cx.new(|cx| {
            let liked_sort = *cx.global::<Models>().liked_tracks_sort_method.read(cx);
            let standalone_sort = LikedTrackSortMethod::ReleaseOrder;
            let resource = Self::new_resource(cx, artist_id, liked_sort, standalone_sort);

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

            cx.subscribe(&playlist_tracker, move |this: &mut Self, _, ev, cx| {
                if let PlaylistEvent::PlaylistUpdated(1) = ev {
                    this.reload(cx);
                }
            })
            .detach();
            let availability = cx.global::<Models>().availability.clone();
            cx.observe(&availability, |_, _, cx| cx.notify()).detach();
            let scan_state = cx.global::<Models>().scan_state.clone();
            cx.observe(&scan_state, |this: &mut Self, state, cx| {
                if matches!(
                    *state.read(cx),
                    ScanEvent::ScanCompleteIdle
                        | ScanEvent::ScanCompleteWatching
                        | ScanEvent::TargetedRescanComplete
                ) {
                    this.reload(cx);
                }
            })
            .detach();

            let grid_views = cx.new(|_| FxHashMap::default());
            let grid_render_counter = cx.new(|_| 0usize);
            cx.observe(&resource, |this: &mut Self, resource, cx| {
                if let Some(data) = resource.read(cx).ready().cloned() {
                    this.apply_data(data, cx);
                }
            })
            .detach();

            ArtistDetailView {
                artist_id,
                artist_name: None,
                album_ids: Vec::new(),
                liked_track_items: Vec::new(),
                standalone_track_items: Vec::new(),
                all_tracks: Arc::new(Vec::new()),
                liked_tracks: Arc::new(Vec::new()),
                standalone_tracks: Arc::new(Vec::new()),
                scroll_handle: ScrollHandle::new(),
                grid_views,
                grid_render_counter,
                nav_model: nav_model.clone(),
                liked_sort,
                standalone_sort,
                resource,
                loaded: false,
            }
        });

        view
    }

    fn new_resource(
        cx: &mut App,
        artist_id: i64,
        liked_sort: LikedTrackSortMethod,
        standalone_sort: LikedTrackSortMethod,
    ) -> Entity<AsyncResource<(i64, LikedTrackSortMethod, LikedTrackSortMethod), ArtistDetailData>>
    {
        let pool = cx.global::<Pool>().0.clone();
        AsyncResource::new(cx, (artist_id, liked_sort, standalone_sort), async move {
            let artist = artists().by_id(artist_id).fetch_optional(&pool).await?;
            let albums = albums()
                .from_artist(artist_id)
                .sort_asc(AlbumColumn::ReleaseDate)
                .fetch_list(&pool)
                .await?;
            let all_tracks = tracks()
                .from_artist(artist_id)
                .sort_release(SortDirection::Ascending)
                .fetch_list(&pool)
                .await?;
            let liked_query = tracks().liked_by_artist(artist_id);
            let liked_tracks = match liked_sort {
                LikedTrackSortMethod::TitleAsc => {
                    liked_query.sort_asc(crate::library::db::TrackColumn::Title)
                }
                LikedTrackSortMethod::TitleDesc => {
                    liked_query.sort_desc(crate::library::db::TrackColumn::Title)
                }
                LikedTrackSortMethod::ReleaseOrder => {
                    liked_query.sort_release(SortDirection::Ascending)
                }
                LikedTrackSortMethod::ReleaseOrderDesc => {
                    liked_query.sort_release(SortDirection::Descending)
                }
                LikedTrackSortMethod::RecentlyAdded => {
                    liked_query.sort_recently_added(SortDirection::Descending)
                }
                LikedTrackSortMethod::RecentlyAddedAsc => {
                    liked_query.sort_recently_added(SortDirection::Ascending)
                }
            }
            .fetch_list(&pool)
            .await?;
            let standalone_query = tracks().standalone_for_artist(artist_id);
            let standalone_tracks = match standalone_sort {
                LikedTrackSortMethod::TitleAsc => {
                    standalone_query.sort_asc(crate::library::db::TrackColumn::Title)
                }
                LikedTrackSortMethod::TitleDesc => {
                    standalone_query.sort_desc(crate::library::db::TrackColumn::Title)
                }
                LikedTrackSortMethod::ReleaseOrder => {
                    standalone_query.sort_release(SortDirection::Ascending)
                }
                LikedTrackSortMethod::ReleaseOrderDesc => {
                    standalone_query.sort_release(SortDirection::Descending)
                }
                LikedTrackSortMethod::RecentlyAdded => {
                    standalone_query.sort_recently_added(SortDirection::Descending)
                }
                LikedTrackSortMethod::RecentlyAddedAsc => {
                    standalone_query.sort_recently_added(SortDirection::Ascending)
                }
            }
            .fetch_list(&pool)
            .await?;
            Ok(ArtistDetailData {
                artist_name: artist.map(|artist| artist.name),
                album_ids: albums
                    .into_iter()
                    .map(|album| (album.id as u32, album.title.to_string()))
                    .collect(),
                all_tracks: Arc::new(all_tracks),
                liked_tracks: Arc::new(liked_tracks),
                standalone_tracks: Arc::new(standalone_tracks),
            })
        })
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.loaded = false;
        let artist_id = self.artist_id;
        let liked_sort = self.liked_sort;
        let standalone_sort = self.standalone_sort;
        let pool = cx.global::<Pool>().0.clone();
        self.resource.update(cx, |resource, cx| {
            resource.load(cx, (artist_id, liked_sort, standalone_sort), async move {
                let artist = artists().by_id(artist_id).fetch_optional(&pool).await?;
                let albums = albums()
                    .from_artist(artist_id)
                    .sort_asc(AlbumColumn::ReleaseDate)
                    .fetch_list(&pool)
                    .await?;
                let all_tracks = tracks()
                    .from_artist(artist_id)
                    .sort_release(SortDirection::Ascending)
                    .fetch_list(&pool)
                    .await?;
                let liked_query = tracks().liked_by_artist(artist_id);
                let liked_tracks = match liked_sort {
                    LikedTrackSortMethod::TitleAsc => {
                        liked_query.sort_asc(crate::library::db::TrackColumn::Title)
                    }
                    LikedTrackSortMethod::TitleDesc => {
                        liked_query.sort_desc(crate::library::db::TrackColumn::Title)
                    }
                    LikedTrackSortMethod::ReleaseOrder => {
                        liked_query.sort_release(SortDirection::Ascending)
                    }
                    LikedTrackSortMethod::ReleaseOrderDesc => {
                        liked_query.sort_release(SortDirection::Descending)
                    }
                    LikedTrackSortMethod::RecentlyAdded => {
                        liked_query.sort_recently_added(SortDirection::Descending)
                    }
                    LikedTrackSortMethod::RecentlyAddedAsc => {
                        liked_query.sort_recently_added(SortDirection::Ascending)
                    }
                }
                .fetch_list(&pool)
                .await?;
                let standalone_query = tracks().standalone_for_artist(artist_id);
                let standalone_tracks = match standalone_sort {
                    LikedTrackSortMethod::TitleAsc => {
                        standalone_query.sort_asc(crate::library::db::TrackColumn::Title)
                    }
                    LikedTrackSortMethod::TitleDesc => {
                        standalone_query.sort_desc(crate::library::db::TrackColumn::Title)
                    }
                    LikedTrackSortMethod::ReleaseOrder => {
                        standalone_query.sort_release(SortDirection::Ascending)
                    }
                    LikedTrackSortMethod::ReleaseOrderDesc => {
                        standalone_query.sort_release(SortDirection::Descending)
                    }
                    LikedTrackSortMethod::RecentlyAdded => {
                        standalone_query.sort_recently_added(SortDirection::Descending)
                    }
                    LikedTrackSortMethod::RecentlyAddedAsc => {
                        standalone_query.sort_recently_added(SortDirection::Ascending)
                    }
                }
                .fetch_list(&pool)
                .await?;
                Ok(ArtistDetailData {
                    artist_name: artist.map(|artist| artist.name),
                    album_ids: albums
                        .into_iter()
                        .map(|album| (album.id as u32, album.title.to_string()))
                        .collect(),
                    all_tracks: Arc::new(all_tracks),
                    liked_tracks: Arc::new(liked_tracks),
                    standalone_tracks: Arc::new(standalone_tracks),
                })
            })
        });
    }

    fn apply_data(&mut self, data: ArtistDetailData, cx: &mut Context<Self>) {
        self.artist_name = data.artist_name;
        self.album_ids = data.album_ids;
        self.all_tracks = data.all_tracks;
        self.set_liked_tracks(data.liked_tracks, cx);
        self.set_standalone_tracks(data.standalone_tracks, cx);
        self.loaded = true;
        cx.notify();
    }

    pub fn update_liked_sort(&mut self, sort_method: LikedTrackSortMethod, cx: &mut Context<Self>) {
        let current_descending = Self::is_descending(self.liked_sort);
        let next_sort = Self::apply_direction(Self::base_sort(sort_method), current_descending);

        if self.liked_sort == next_sort {
            return;
        }

        self.liked_sort = next_sort;
        self.sync_sort_with_model(cx);

        self.reload(cx);
    }

    fn set_liked_tracks(&mut self, liked_tracks: Arc<Vec<Track>>, cx: &mut Context<Self>) {
        self.rebuild_liked_tracks(liked_tracks, cx);
        cx.notify();
    }

    fn rebuild_liked_tracks(&mut self, liked_tracks: Arc<Vec<Track>>, cx: &mut Context<Self>) {
        self.liked_tracks = liked_tracks;

        self.liked_track_items = self
            .liked_tracks
            .iter()
            .enumerate()
            .map(|(index, track): (usize, &Track)| {
                TrackItem::new(
                    cx,
                    track.clone(),
                    index,
                    false,
                    ArtistNameVisibility::OnlyIfDifferent(self.artist_name.clone()),
                    TrackItemLeftField::Art,
                    None,
                    NumberDisplayMode::Standard,
                    None,
                    Some(self.liked_tracks.clone()),
                    false,
                    false,
                )
            })
            .collect();
    }

    fn toggle_liked_sort_order(&mut self, cx: &mut Context<Self>) {
        self.liked_sort = Self::toggled_sort(self.liked_sort);
        self.sync_sort_with_model(cx);
        self.reload(cx);
    }

    fn update_standalone_sort(
        &mut self,
        sort_method: LikedTrackSortMethod,
        cx: &mut Context<Self>,
    ) {
        let current_descending = Self::is_descending(self.standalone_sort);
        let next_sort = Self::apply_direction(Self::base_sort(sort_method), current_descending);

        if self.standalone_sort == next_sort {
            return;
        }

        self.standalone_sort = next_sort;

        self.reload(cx);
    }

    fn set_standalone_tracks(
        &mut self,
        standalone_tracks: Arc<Vec<Track>>,
        cx: &mut Context<Self>,
    ) {
        self.rebuild_standalone_tracks(standalone_tracks, cx);
        cx.notify();
    }

    fn rebuild_standalone_tracks(
        &mut self,
        standalone_tracks: Arc<Vec<Track>>,
        cx: &mut Context<Self>,
    ) {
        self.standalone_tracks = standalone_tracks;

        self.standalone_track_items = self
            .standalone_tracks
            .iter()
            .enumerate()
            .map(|(index, track): (usize, &Track)| {
                TrackItem::new(
                    cx,
                    track.clone(),
                    index,
                    false,
                    ArtistNameVisibility::OnlyIfDifferent(self.artist_name.clone()),
                    TrackItemLeftField::Art,
                    None,
                    NumberDisplayMode::Standard,
                    None,
                    Some(self.standalone_tracks.clone()),
                    false,
                    false,
                )
            })
            .collect();
    }

    fn toggle_standalone_sort_order(&mut self, cx: &mut Context<Self>) {
        self.standalone_sort = Self::toggled_sort(self.standalone_sort);
        self.reload(cx);
    }

    fn base_sort(sort_method: LikedTrackSortMethod) -> LikedTrackSortMethod {
        match sort_method {
            LikedTrackSortMethod::TitleAsc | LikedTrackSortMethod::TitleDesc => {
                LikedTrackSortMethod::TitleAsc
            }
            LikedTrackSortMethod::ReleaseOrder | LikedTrackSortMethod::ReleaseOrderDesc => {
                LikedTrackSortMethod::ReleaseOrder
            }
            LikedTrackSortMethod::RecentlyAdded | LikedTrackSortMethod::RecentlyAddedAsc => {
                LikedTrackSortMethod::RecentlyAdded
            }
        }
    }

    fn apply_direction(
        base_sort_method: LikedTrackSortMethod,
        descending: bool,
    ) -> LikedTrackSortMethod {
        match base_sort_method {
            LikedTrackSortMethod::TitleAsc | LikedTrackSortMethod::TitleDesc => {
                if descending {
                    LikedTrackSortMethod::TitleDesc
                } else {
                    LikedTrackSortMethod::TitleAsc
                }
            }
            LikedTrackSortMethod::ReleaseOrder | LikedTrackSortMethod::ReleaseOrderDesc => {
                if descending {
                    LikedTrackSortMethod::ReleaseOrderDesc
                } else {
                    LikedTrackSortMethod::ReleaseOrder
                }
            }
            LikedTrackSortMethod::RecentlyAdded | LikedTrackSortMethod::RecentlyAddedAsc => {
                if descending {
                    LikedTrackSortMethod::RecentlyAdded
                } else {
                    LikedTrackSortMethod::RecentlyAddedAsc
                }
            }
        }
    }

    fn is_descending(sort_method: LikedTrackSortMethod) -> bool {
        matches!(
            sort_method,
            LikedTrackSortMethod::TitleDesc
                | LikedTrackSortMethod::ReleaseOrderDesc
                | LikedTrackSortMethod::RecentlyAdded
        )
    }

    fn toggled_sort(sort_method: LikedTrackSortMethod) -> LikedTrackSortMethod {
        match sort_method {
            LikedTrackSortMethod::TitleAsc => LikedTrackSortMethod::TitleDesc,
            LikedTrackSortMethod::TitleDesc => LikedTrackSortMethod::TitleAsc,
            LikedTrackSortMethod::ReleaseOrder => LikedTrackSortMethod::ReleaseOrderDesc,
            LikedTrackSortMethod::ReleaseOrderDesc => LikedTrackSortMethod::ReleaseOrder,
            LikedTrackSortMethod::RecentlyAdded => LikedTrackSortMethod::RecentlyAddedAsc,
            LikedTrackSortMethod::RecentlyAddedAsc => LikedTrackSortMethod::RecentlyAdded,
        }
    }

    fn sync_sort_with_model(&self, cx: &mut Context<Self>) {
        let liked_tracks_sort_method = cx.global::<Models>().liked_tracks_sort_method.clone();
        liked_tracks_sort_method.update(cx, |value, _| *value = self.liked_sort);
    }
}

impl Render for ArtistDetailView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.loaded {
            return div().into_any_element();
        }
        let theme = cx.global::<Theme>();
        let entity = cx.entity();
        let standalone_entity = entity.clone();

        let scroll_handle = self.scroll_handle.clone();
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();
        let padding = detail_view_padding(cx);
        let grid_min_item_width = crate::settings::interface::clamp_grid_min_item_width(
            settings.interface.grid_min_item_width,
        );

        let album_count = self.album_ids.len();
        let album_ids = self.album_ids.clone();
        let grid_views_model = self.grid_views.clone();
        let grid_render_counter = self.grid_render_counter.clone();
        let nav_model = self.nav_model.clone();

        let is_playing =
            cx.global::<PlaybackInfo>().playback_state.read(cx) == &PlaybackState::Playing;
        let availability = snapshot(cx);

        let current_track_in_artist = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .clone()
            .is_some_and(|current_track| {
                self.all_tracks.iter().any(|track| {
                    current_track == track.location
                        && availability.is_track_path_available(&track.location)
                })
            });
        let has_available_artist_tracks = has_available_tracks(cx, self.all_tracks.as_ref());

        let current_track_in_liked = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .clone()
            .is_some_and(|current_track| {
                self.liked_tracks.iter().any(|track| {
                    current_track == track.location
                        && availability.is_track_path_available(&track.location)
                })
            });
        let has_available_liked_tracks = has_available_tracks(cx, self.liked_tracks.as_ref());

        let current_track_in_standalone = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .clone()
            .is_some_and(|current_track| {
                self.standalone_tracks.iter().any(|track| {
                    current_track == track.location
                        && availability.is_track_path_available(&track.location)
                })
            });
        let has_available_standalone_tracks =
            has_available_tracks(cx, self.standalone_tracks.as_ref());

        let liked_track_header =
            if !self.liked_track_items.is_empty() {
                Some(
                    div()
                        .border_t_1()
                        .border_color(theme.border_color)
                        .px(padding)
                        .pt(px(10.0))
                        .pb(px(5.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(18.0))
                                .my_auto()
                                .child(tr!("ARTIST_LIKED_TRACKS", "Liked Tracks")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_between()
                                .pb(px(13.0))
                                .child(playback_controls(
                                    "artist-liked",
                                    has_available_liked_tracks,
                                    current_track_in_liked,
                                    is_playing,
                                    {
                                        let liked_tracks = self.liked_tracks.clone();
                                        let availability = availability.clone();
                                        move |cx| {
                                            liked_tracks
                                                .iter()
                                                .filter(|track| {
                                                    availability
                                                        .is_track_path_available(&track.location)
                                                })
                                                .map(|track| {
                                                    QueueItemData::new(
                                                        cx,
                                                        track.location.clone(),
                                                        Some(track.id),
                                                        track.album_id,
                                                    )
                                                })
                                                .collect()
                                        }
                                    },
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .gap(px(12.0))
                                        .items_center()
                                        .child(
                                            button()
                                                .id("artist-liked-sort-direction-button")
                                                .size(ButtonSize::Large)
                                                .on_click(cx.listener(
                                                    |this: &mut ArtistDetailView, _, _, cx| {
                                                        this.toggle_liked_sort_order(cx);
                                                    },
                                                ))
                                                .child(
                                                    icon(if Self::is_descending(self.liked_sort) {
                                                        SORT_DESCENDING
                                                    } else {
                                                        SORT_ASCENDING
                                                    })
                                                    .text_color(theme.text_secondary)
                                                    .size(REGULAR_BUTTON_ICON_SIZE),
                                                )
                                                .tooltip(if Self::is_descending(self.liked_sort) {
                                                    build_tooltip(tr!(
                                                        "SORT_ASCENDING",
                                                        "Sort Ascending"
                                                    ))
                                                } else {
                                                    build_tooltip(tr!(
                                                        "SORT_DESCENDING",
                                                        "Sort Descending"
                                                    ))
                                                }),
                                        )
                                        .child(
                                            dropdown::<LikedTrackSortMethod>(
                                                "artist-liked-sort-dropdown",
                                            )
                                            .option(
                                                LikedTrackSortMethod::RecentlyAdded,
                                                tr!("SORT_RECENTLY_ADDED", "Recently Added"),
                                            )
                                            .option(
                                                LikedTrackSortMethod::TitleAsc,
                                                tr!("SORT_TITLE", "Title"),
                                            )
                                            .option(
                                                LikedTrackSortMethod::ReleaseOrder,
                                                tr!("SORT_RELEASE_ORDER", "Release Order"),
                                            )
                                            .selected(Self::base_sort(self.liked_sort))
                                            .w(px(200.0))
                                            .on_change(move |sort_method, _, cx| {
                                                entity.update(cx, |this, cx| {
                                                    this.update_liked_sort(*sort_method, cx);
                                                });
                                            }),
                                        ),
                                ),
                        ),
                )
            } else {
                None
            };

        let standalone_track_header = if !self.standalone_track_items.is_empty() {
            Some(
                div()
                    .border_t_1()
                    .border_color(theme.border_color)
                    .pl(padding)
                    .pr(px(12.0))
                    .pt(px(10.0))
                    .pb(px(5.0))
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(18.0))
                            .my_auto()
                            .child(tr!("TRACKS")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .pb(px(13.0))
                            .child(playback_controls(
                                "artist-standalone",
                                has_available_standalone_tracks,
                                current_track_in_standalone,
                                is_playing,
                                {
                                    let standalone_tracks = self.standalone_tracks.clone();
                                    let availability = availability.clone();
                                    move |cx| {
                                        standalone_tracks
                                            .iter()
                                            .filter(|track| {
                                                availability
                                                    .is_track_path_available(&track.location)
                                            })
                                            .map(|track| {
                                                QueueItemData::new(
                                                    cx,
                                                    track.location.clone(),
                                                    Some(track.id),
                                                    track.album_id,
                                                )
                                            })
                                            .collect()
                                    }
                                },
                            ))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(12.0))
                                    .items_center()
                                    .child(
                                        button()
                                            .size(ButtonSize::Large)
                                            .id("artist-standalone-sort-direction-button")
                                            .on_click(cx.listener(
                                                |this: &mut ArtistDetailView, _, _, cx| {
                                                    this.toggle_standalone_sort_order(cx);
                                                },
                                            ))
                                            .child(
                                                icon(
                                                    if Self::is_descending(self.standalone_sort) {
                                                        SORT_DESCENDING
                                                    } else {
                                                        SORT_ASCENDING
                                                    },
                                                )
                                                .text_color(theme.text_secondary)
                                                .size(REGULAR_BUTTON_ICON_SIZE),
                                            )
                                            .tooltip(
                                                if Self::is_descending(self.standalone_sort) {
                                                    build_tooltip(tr!("SORT_ASCENDING"))
                                                } else {
                                                    build_tooltip(tr!("SORT_DESCENDING"))
                                                },
                                            ),
                                    )
                                    .child(
                                        dropdown::<LikedTrackSortMethod>(
                                            "artist-standalone-sort-dropdown",
                                        )
                                        .option(
                                            LikedTrackSortMethod::RecentlyAdded,
                                            tr!("SORT_RECENTLY_ADDED"),
                                        )
                                        .option(LikedTrackSortMethod::TitleAsc, tr!("SORT_TITLE"))
                                        .option(
                                            LikedTrackSortMethod::ReleaseOrder,
                                            tr!("SORT_RELEASE_ORDER"),
                                        )
                                        .selected(Self::base_sort(self.standalone_sort))
                                        .w(px(200.0))
                                        .on_change(
                                            move |sort_method, _, cx| {
                                                standalone_entity.update(cx, |this, cx| {
                                                    this.update_standalone_sort(*sort_method, cx);
                                                });
                                            },
                                        ),
                                    ),
                            ),
                    ),
            )
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .w_full()
            .max_h_full()
            .relative()
            .overflow_hidden()
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .child(
                div()
                    .flex()
                    .w_full()
                    .max_h_full()
                    .relative()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("artist-detail-view")
                            .overflow_y_scroll()
                            .track_scroll(&scroll_handle)
                            .pb(px(12.0))
                            .w_full()
                            .flex_shrink(1.0)
                            .overflow_x_hidden()
                            .child(
                                div()
                                    // 48px clears the floating header bar; the rest
                                    // keeps the heading's top gap square with the
                                    // horizontal padding.
                                    .pt(px(48.0) + padding)
                                    .pl(padding)
                                    .pr(px(12.0))
                                    .w_full()
                                    .relative()
                                    .child(LibraryViewHeader::detail("artist_detail_close"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::EXTRA_BOLD)
                                            .text_size(rems(2.5))
                                            .line_height(rems(2.75))
                                            .overflow_x_hidden()
                                            .pb(px(10.0))
                                            .w_full()
                                            .text_ellipsis()
                                            .when_some(self.artist_name.clone(), |this, name| {
                                                this.child(name)
                                            }),
                                    )
                                    .when(!self.all_tracks.is_empty(), |this| {
                                        this.child(div().pb(padding).child(playback_controls(
                                            "artist",
                                            has_available_artist_tracks,
                                            current_track_in_artist,
                                            is_playing,
                                            {
                                                let all_tracks = self.all_tracks.clone();
                                                let availability = availability.clone();
                                                move |cx| {
                                                    all_tracks
                                                        .iter()
                                                        .filter(|track| {
                                                            availability.is_track_path_available(
                                                                &track.location,
                                                            )
                                                        })
                                                        .map(|track| {
                                                            QueueItemData::new(
                                                                cx,
                                                                track.location.clone(),
                                                                Some(track.id),
                                                                track.album_id,
                                                            )
                                                        })
                                                        .collect()
                                                }
                                            },
                                        )))
                                    }),
                            )
                            .when(album_count > 0, |this| {
                                let handler: Option<Rc<GridHandler>> =
                                    Some(Rc::new(move |cx, id| {
                                        nav_model.update(cx, |_, cx| {
                                            cx.emit(ViewSwitchMessage::Release(*id as i64, None));
                                        });
                                    }));

                                this.child(
                                    div()
                                        .border_t_1()
                                        .border_color(theme.border_color)
                                        .pl(padding)
                                        .pr(px(12.0))
                                        .pt(px(10.0))
                                        .font_weight(FontWeight::BOLD)
                                        .text_size(px(18.0))
                                        .child(tr!("ARTIST_ALBUMS", "Albums")),
                                )
                                .child(
                                    // Grid items have 8px of internal padding, so this
                                    // wrapper keeps the album art aligned with the
                                    // section header above it.
                                    div()
                                        .pl(padding - px(8.0))
                                        .pr(px(4.0))
                                        .pt(px(2.0))
                                        .pb(px(10.0))
                                        .w_full()
                                        .child(
                                            uniform_grid(
                                                "artist-albums-grid",
                                                album_count,
                                                None,
                                                move |idx, item_width, _, cx| {
                                                    prune_views(
                                                        &grid_views_model,
                                                        &grid_render_counter,
                                                        idx,
                                                        cx,
                                                    );

                                                    let item_id = album_ids[idx].0;

                                                    let view = create_or_retrieve_view(
                                                        &grid_views_model,
                                                        idx,
                                                        |cx| {
                                                            GridItem::<Album, AlbumColumn>::new(
                                                                cx,
                                                                item_id,
                                                                idx,
                                                                handler.clone(),
                                                                AlbumContextMenuContext {
                                                                    show_go_to_artist: false,
                                                                },
                                                                GridContext::Standalone,
                                                            )
                                                        },
                                                        cx,
                                                    );

                                                    view.update(cx, |item, cx| {
                                                        item.set_image_target(item_width, cx);
                                                    });

                                                    div().size_full().child(view).into_any_element()
                                                },
                                            )
                                            .min_item_width(px(grid_min_item_width))
                                            .gap(px(0.0))
                                            .auto_height(),
                                        ),
                                )
                            })
                            .when_some(liked_track_header, |this, header| {
                                this.child(header).child(
                                    div()
                                        .w_full()
                                        .border_t_1()
                                        .border_color(theme.border_color)
                                        .children(
                                            self.liked_track_items
                                                .iter()
                                                .map(|item| div().h(px(40.0)).child(item.clone())),
                                        ),
                                )
                            })
                            .when_some(standalone_track_header, |this, header| {
                                this.child(header).child(
                                    div()
                                        .w_full()
                                        .border_t_1()
                                        .border_color(theme.border_color)
                                        .children(
                                            self.standalone_track_items
                                                .iter()
                                                .map(|item| div().h(px(40.0)).child(item.clone())),
                                        ),
                                )
                            }),
                    )
                    .child(
                        floating_scrollbar("artist_detail_scrollbar", scroll_handle).right(px(4.0)),
                    ),
            )
            .into_any_element()
    }
}
