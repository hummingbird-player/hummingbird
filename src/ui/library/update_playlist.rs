use std::sync::Arc;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use nucleo::Utf32String;

use crate::{
    library::{
        db::LibraryAccess,
        playlist::import_playlist,
        types::{Playlist, PlaylistType},
    },
    ui::components::{
        icons::{PLAYLIST, PLAYLIST_ADD, STAR_FILLED},
        modal::modal,
        palette::{ExtraItem, ExtraItemProvider, FinderItemLeft, Palette, PaletteItem},
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
}

impl UpdatePlaylist {
    pub fn new(cx: &mut App, show: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&show, move |this: &mut Self, _, cx| {
                this.palette.update(cx, |this, cx| {
                    let new_playlists = (*cx.get_all_playlists().unwrap())
                        .clone()
                        .into_iter()
                        .map(Arc::new)
                        .collect::<Vec<_>>();

                    cx.emit(new_playlists);

                    this.reset(cx);
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

            let items = (*cx.get_all_playlists().unwrap())
                .clone()
                .into_iter()
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
                        let playlist_id = cx.create_playlist(&name_string).unwrap();

                        import_playlist(cx, playlist_id);

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
