use std::sync::{Arc, RwLock};

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, anchored, div, px,
};
use nucleo::Utf32String;
use tracing::error;

use crate::{
    library::{
        db::{self, LibraryAccess},
        types::Playlist,
    },
    ui::{
        app::Pool,
        components::{
            icons::PLAYLIST_ADD,
            modal::modal,
            palette::{ExtraItem, ExtraItemProvider, FinderItemLeft, Palette, PaletteItem},
        },
        models::{Models, PlaylistEvent},
    },
};

#[derive(Clone, Debug, PartialEq)]
enum TrackList {
    Single(i64),
    Multi(Vec<i64>),
}

impl TrackList {
    fn from_ids(ids: Vec<i64>) -> Self {
        if ids.len() == 1 {
            TrackList::Single(ids[0])
        } else {
            TrackList::Multi(ids)
        }
    }

    fn first(&self) -> i64 {
        match self {
            TrackList::Single(id) => *id,
            TrackList::Multi(ids) => ids[0],
        }
    }

    fn is_multi(&self) -> bool {
        matches!(self, TrackList::Multi(ids) if ids.len() > 1)
    }

    fn ids(&self) -> &[i64] {
        match self {
            TrackList::Single(id) => std::slice::from_ref(id),
            TrackList::Multi(ids) => ids,
        }
    }
}

type SharedTrackList = Arc<RwLock<TrackList>>;

fn read_track_list(shared: &SharedTrackList) -> TrackList {
    shared.read().expect("poisoned track_list lock").clone()
}

impl PaletteItem for (TrackList, Playlist) {
    fn left_content(&self, cx: &mut App) -> Option<FinderItemLeft> {
        self.1.left_content(cx)
    }

    fn middle_content(&self, cx: &mut App) -> SharedString {
        if self.0.is_multi() {
            tr!("ADD_TO_SELECTED_PLAYLIST", name = self.1.name.0.as_str()).into()
        } else {
            let track_id = self.0.first();
            let has_track = cx.playlist_has_track(self.1.id, track_id).ok().flatten();

            if has_track.is_none() {
                tr!(
                    "ADD_TO_SELECTED_PLAYLIST",
                    "Add to {{name}}",
                    name = self.1.name.0.as_str()
                )
                .into()
            } else {
                tr!(
                    "REMOVE_FROM_SELECTED_PLAYLIST",
                    "Remove from {{name}}",
                    name = self.1.name.0.as_str()
                )
                .into()
            }
        }
    }

    fn right_content(&self, cx: &mut App) -> Option<SharedString> {
        self.1.right_content(cx)
    }
}

type MatcherFunc = Box<dyn Fn(&Arc<(TrackList, Playlist)>, &mut App) -> Utf32String + 'static>;
type OnAccept = Box<dyn Fn(&Arc<(TrackList, Playlist)>, &mut App) + 'static>;

pub struct AddToPlaylist {
    show: Entity<bool>,
    palette: Entity<Palette<(TrackList, Playlist), MatcherFunc, OnAccept>>,
    track_list: SharedTrackList,
}

impl AddToPlaylist {
    pub fn new(cx: &mut App, show: Entity<bool>, track_ids: Vec<i64>) -> Entity<Self> {
        cx.new(|cx| {
            let track_list: SharedTrackList = Arc::new(RwLock::new(TrackList::from_ids(track_ids)));

            let track_list_for_observe = track_list.clone();
            cx.observe(&show, move |this: &mut Self, _, cx| {
                let current = read_track_list(&track_list_for_observe);
                this.palette.update(cx, |palette, cx| {
                    let new_playlists = (*cx.get_all_playlists().unwrap())
                        .clone()
                        .into_iter()
                        .map(|playlist| (current.clone(), playlist))
                        .map(Arc::new)
                        .collect::<Vec<_>>();

                    cx.emit(new_playlists);

                    palette.reset(cx);
                });

                cx.notify();
            })
            .detach();

            let matcher: MatcherFunc = Box::new(|playlist, _| playlist.1.name.0.to_string().into());

            let show_clone = show.clone();

            let on_accept: OnAccept = Box::new(move |playlist, cx| {
                let track_ids = playlist.0.ids().to_vec();
                let playlist_id = playlist.1.id;

                if track_ids.len() == 1 {
                    let track_id = track_ids[0];
                    let has_track = cx.playlist_has_track(playlist_id, track_id).ok().flatten();

                    let pool = cx.global::<Pool>().0.clone();
                    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

                    cx.spawn(async move |cx| {
                        let task = if let Some(id) = has_track {
                            crate::RUNTIME
                                .spawn(async move { db::remove_playlist_item(&pool, id).await })
                        } else {
                            crate::RUNTIME.spawn(async move {
                                db::add_playlist_item(&pool, playlist_id, track_id)
                                    .await
                                    .map(|_| ())
                            })
                        };

                        match task.await {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => {
                                error!("could not remove/add track from playlist: {err:?}");
                                return;
                            }
                            Err(err) => {
                                error!("remove/add from playlist task panicked: {err:?}");
                                return;
                            }
                        }

                        playlist_tracker.update(cx, |_, cx| {
                            cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                        });
                    })
                    .detach();
                } else {
                    let pool = cx.global::<Pool>().0.clone();
                    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

                    cx.spawn(async move |cx| {
                        let task = crate::RUNTIME.spawn(async move {
                            for track_id in &track_ids {
                                db::add_playlist_item(&pool, playlist_id, *track_id).await?;
                            }
                            Ok::<(), sqlx::Error>(())
                        });

                        match task.await {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => {
                                error!("could not add tracks to playlist: {err:?}");
                                return;
                            }
                            Err(err) => {
                                error!("add tracks to playlist task panicked: {err:?}");
                                return;
                            }
                        }

                        playlist_tracker.update(cx, |_, cx| {
                            cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                        });
                    })
                    .detach();
                }

                show_clone.write(cx, false);
            });

            let initial_track_list = read_track_list(&track_list);
            let items = (*cx.get_all_playlists().unwrap())
                .clone()
                .into_iter()
                .map(|playlist| (initial_track_list.clone(), playlist))
                .map(Arc::new)
                .collect();

            let palette = Palette::new(cx, items, matcher, on_accept, &show);

            let track_list_for_create = track_list.clone();
            let show_for_create = show.clone();
            let provider: ExtraItemProvider = Arc::new(move |query: &str| {
                let name = query.trim();
                if name.is_empty() {
                    return Vec::new();
                }

                let name_string = name.to_string();
                let display = tr!("CREATE_PLAYLIST", name = name_string);

                let show_clone2 = show_for_create.clone();
                let create_track_ids = read_track_list(&track_list_for_create).ids().to_vec();

                vec![ExtraItem {
                    left: Some(FinderItemLeft::Icon(PLAYLIST_ADD.into())),
                    middle: display.into(),
                    right: None,
                    on_accept: Arc::new(move |cx| {
                        let pool = cx.global::<Pool>().0.clone();
                        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                        let name_string = name_string.clone();
                        let create_track_ids = create_track_ids.clone();

                        cx.spawn(async move |cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                let playlist_id = db::create_playlist(&pool, &name_string).await?;
                                for track_id in &create_track_ids {
                                    db::add_playlist_item(&pool, playlist_id, *track_id).await?;
                                }
                                Ok::<i64, sqlx::Error>(playlist_id)
                            });

                            let playlist_id = match task.await {
                                Ok(Ok(id)) => id,
                                Ok(Err(err)) => {
                                    tracing::error!(
                                        "could not create playlist and add track: {err:?}"
                                    );
                                    return;
                                }
                                Err(err) => {
                                    tracing::error!("create playlist task panicked: {err:?}");
                                    return;
                                }
                            };

                            playlist_tracker.update(cx, |_, cx| {
                                cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                            });
                        })
                        .detach();

                        show_clone2.write(cx, false);
                    }),
                }]
            });

            cx.update_entity(&palette, |palette, cx| {
                palette.register_extra_provider(provider.clone(), cx);
            });

            Self {
                show,
                palette,
                track_list,
            }
        })
    }

    pub fn set_track_ids(&self, track_ids: Vec<i64>) {
        *self.track_list.write().expect("poisoned track_list lock") =
            TrackList::from_ids(track_ids);
    }
}

impl Render for AddToPlaylist {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show = self.show.clone();
        let palette = self.palette.clone();
        let show_read = *self.show.read(cx);

        if show_read {
            cx.update_entity(&palette, |palette, cx| {
                palette.focus(window, cx);
            });

            modal()
                .child(div().w(px(550.0)).h(px(300.0)).child(palette.clone()))
                .on_exit(move |_, cx| {
                    show.update(cx, |show, cx| {
                        *show = false;
                        cx.update_entity(&palette, |palette, cx| {
                            palette.reset(cx);
                        });
                        cx.notify();
                    })
                })
                .into_any_element()
        } else {
            anchored().into_any_element()
        }
    }
}
