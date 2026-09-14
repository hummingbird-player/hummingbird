use std::sync::Arc;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use nucleo::Utf32String;

use crate::{
    library::{
        db::{self, playlists},
        playlist::import_playlist,
        types::{Playlist, PlaylistType},
    },
    ui::{
        app::Pool,
        components::{
            async_resource::AsyncResource,
            icons::{PLAYLIST, PLAYLIST_ADD, STAR_FILLED},
            modal::modal,
            palette::{ExtraItem, ExtraItemProvider, FinderItemLeft, Palette, PaletteItem},
        },
    },
};

impl PaletteItem for Playlist {
    fn left_content(&self, _: &mut App) -> Option<FinderItemLeft> {
        Some(FinderItemLeft::Icon(match self.playlist_type {
            PlaylistType::User => PLAYLIST.into(),
            PlaylistType::System => STAR_FILLED.into(),
        }))
    }

    fn middle_content(&self, _: &mut App) -> SharedString {
        tr!(
            "UPDATE_PLAYLIST",
            "Update {{name}}",
            name = self.name.0.as_str()
        )
        .into()
    }

    fn right_content(&self, _: &mut App) -> Option<SharedString> {
        None
    }
}

type MatcherFunc = Box<dyn Fn(&Arc<Playlist>, &mut App) -> Utf32String + 'static>;
type OnAccept = Box<dyn Fn(&Arc<Playlist>, &mut App) + 'static>;

pub struct UpdatePlaylist {
    show: Entity<bool>,
    palette: Entity<Palette<Playlist, MatcherFunc, OnAccept>>,
    playlists: Entity<AsyncResource<(), Arc<Vec<Playlist>>>>,
}

impl UpdatePlaylist {
    pub fn new(cx: &mut App, show: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            let pool = cx.global::<Pool>().0.clone();
            let playlists_resource = AsyncResource::new(cx, (), async move {
                Ok(Arc::new(playlists().fetch_list(&pool).await?))
            });
            cx.observe(&show, move |this: &mut Self, _, cx| {
                let pool = cx.global::<Pool>().0.clone();
                this.playlists.update(cx, |resource, cx| {
                    resource.load(cx, (), async move {
                        Ok(Arc::new(playlists().fetch_list(&pool).await?))
                    });
                });
                cx.notify();
            })
            .detach();

            let matcher: MatcherFunc = Box::new(|playlist, _| playlist.name.0.to_string().into());

            let show_clone = show.clone();

            let on_accept: OnAccept = Box::new(move |playlist, cx| {
                import_playlist(cx, playlist.id);
                show_clone.write(cx, false);
            });

            let palette = Palette::new(cx, Vec::new(), matcher, on_accept, &show);
            let palette_for_resource = palette.clone();
            cx.observe(&playlists_resource, move |_, resource, cx| {
                let Some(items) = resource.read(cx).ready().cloned() else {
                    return;
                };
                palette_for_resource.update(cx, |palette, cx| {
                    cx.emit(items.iter().cloned().map(Arc::new).collect::<Vec<_>>());
                    palette.reset(cx);
                });
            })
            .detach();

            let show_for_create = show.clone();
            let provider: ExtraItemProvider = Arc::new(move |query: &str| {
                let name = query.trim();
                if name.is_empty() {
                    return Vec::new();
                }

                let name_string = name.to_string();

                let display = tr!(
                    "CREATE_PLAYLIST",
                    "Create new playlist '{{name}}'",
                    name = name
                );

                let show_clone2 = show_for_create.clone();

                vec![ExtraItem {
                    left: Some(FinderItemLeft::Icon(PLAYLIST_ADD.into())),
                    middle: display.into(),
                    right: None,
                    on_accept: Arc::new(move |cx| {
                        let pool = cx.global::<Pool>().0.clone();
                        let name_string = name_string.clone();
                        cx.spawn(async move |cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                db::create_playlist(&pool, &name_string).await
                            });
                            match task.await {
                                Ok(Ok(playlist_id)) => {
                                    cx.update(|cx| import_playlist(cx, playlist_id))
                                }
                                Ok(Err(err)) => {
                                    tracing::error!("could not create playlist: {err:?}")
                                }
                                Err(err) => {
                                    tracing::error!("create playlist task panicked: {err:?}")
                                }
                            }
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
                playlists: playlists_resource,
            }
        })
    }
}

impl Render for UpdatePlaylist {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show = self.show.clone();
        let palette = self.palette.clone();
        let show_read = *self.show.read(cx);

        if show_read {
            cx.update_entity(&palette, |palette, cx| {
                palette.focus(window, cx);
            });

            modal()
                .transparent()
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
            div().into_any_element()
        }
    }
}
