use std::sync::Arc;

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

// i64 here is the track ID
impl PaletteItem for (i64, Playlist) {
    fn left_content(&self, cx: &mut App) -> Option<FinderItemLeft> {
        self.1.left_content(cx)
    }

    fn middle_content(&self, cx: &mut App) -> SharedString {
        let has_track = cx.playlist_has_track(self.1.id, self.0).ok().flatten();

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

    fn right_content(&self, cx: &mut App) -> Option<SharedString> {
        self.1.right_content(cx)
    }
}

type MatcherFunc = Box<dyn Fn(&Arc<(i64, Playlist)>, &mut App) -> Utf32String + 'static>;
type OnAccept = Box<dyn Fn(&Arc<(i64, Playlist)>, &mut App) + 'static>;

pub struct AddToPlaylist {
    show: Entity<bool>,
    palette: Entity<Palette<(i64, Playlist), MatcherFunc, OnAccept>>,
}

impl AddToPlaylist {
    pub fn new(cx: &mut App, show: Entity<bool>, track_id: i64) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&show, move |this: &mut Self, _, cx| {
                this.palette.update(cx, |this, cx| {
                    let new_playlists = (*cx.get_all_playlists().unwrap())
                        .clone()
                        .into_iter()
                        .map(|playlist| (track_id, playlist))
                        .map(Arc::new)
                        .collect::<Vec<_>>();

                    cx.emit(new_playlists);

                    this.reset(cx);
                });

                cx.notify();
            })
            .detach();

            let matcher: MatcherFunc = Box::new(|playlist, _| playlist.1.name.0.to_string().into());

            let show_clone = show.clone();

            let on_accept: OnAccept = Box::new(move |playlist, cx| {
                let has_track = cx
                    .playlist_has_track(playlist.1.id, track_id)
                    .ok()
                    .flatten();

                let pool = cx.global::<Pool>().0.clone();
                let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                let playlist_id = playlist.1.id;

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

                show_clone.write(cx, false);
            });

            let items = (*cx.get_all_playlists().unwrap())
                .clone()
                .into_iter()
                .map(|playlist| (track_id, playlist))
                .map(Arc::new)
                .collect();

            let palette = Palette::new(cx, items, matcher, on_accept, &show);

            let show_for_create = show.clone();
            let provider: ExtraItemProvider = Arc::new(move |query: &str| {
                let name = query.trim();
                if name.is_empty() {
                    return Vec::new();
                }

                let name_string = name.to_string();
                let display = tr!("CREATE_PLAYLIST", name = name_string);

                let show_clone2 = show_for_create.clone();

                vec![ExtraItem {
                    left: Some(FinderItemLeft::Icon(PLAYLIST_ADD.into())),
                    middle: display.into(),
                    right: None,
                    on_accept: Arc::new(move |cx| {
                        let pool = cx.global::<Pool>().0.clone();
                        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                        let name_string = name_string.clone();

                        cx.spawn(async move |cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                let playlist_id = db::create_playlist(&pool, &name_string).await?;
                                db::add_playlist_item(&pool, playlist_id, track_id).await?;
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

            Self { show, palette }
        })
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
