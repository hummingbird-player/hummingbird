use std::sync::Arc;

use cntp_i18n::tr;
use gpui::{App, SharedString};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use super::{Album, ArtistWithCounts, Track};
use crate::{
    library::db::{AlbumMethod, AlbumSortMethod, ArtistSortMethod, LibraryAccess, TrackSortMethod},
    ui::availability::{
        album_has_available_tracks, artist_has_available_tracks, is_track_available,
    },
    ui::components::{
        drag_drop::{AlbumDragData, TrackDragData},
        table::table_data::{Column, GridContext, TableData, TableDragData, TableSort},
    },
    ui::library::context_menus::{
        AlbumContextMenuContext, TrackContextMenuContext, album_menu_for_table,
        track_menu_for_table,
    },
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum AlbumColumn {
    Title,
    Artist,
    Date,
    Label,
    CatalogNumber,
}

impl Column for AlbumColumn {
    fn get_column_name(&self) -> SharedString {
        match self {
            AlbumColumn::Title => tr!("COLUMN_TITLE", "Title").into(),
            AlbumColumn::Artist => tr!("COLUMN_ARTIST", "Artist").into(),
            AlbumColumn::Date => tr!("COLUMN_DATE", "Date").into(),
            AlbumColumn::Label => tr!("COLUMN_LABEL", "Label").into(),
            AlbumColumn::CatalogNumber => tr!("COLUMN_CATALOG_NUMBER", "Catalog Number").into(),
        }
    }

    fn is_hideable(&self) -> bool {
        !matches!(self, AlbumColumn::Title)
    }

    fn all_columns() -> &'static [Self] {
        &[
            AlbumColumn::Title,
            AlbumColumn::Artist,
            AlbumColumn::Date,
            AlbumColumn::Label,
            AlbumColumn::CatalogNumber,
        ]
    }
}

impl TableData<AlbumColumn> for Album {
    type Identifier = (u32, String);
    type ContextMenuContext = AlbumContextMenuContext;

    fn get_table_name() -> SharedString {
        tr!("TABLE_ALBUMS", "Albums").into()
    }

    fn get_rows(
        cx: &mut gpui::App,
        sort: Option<TableSort<AlbumColumn>>,
    ) -> anyhow::Result<Vec<Self::Identifier>> {
        let sort_method = match sort {
            Some(TableSort {
                column: AlbumColumn::Title,
                ascending: true,
            }) => AlbumSortMethod::TitleAsc,
            Some(TableSort {
                column: AlbumColumn::Title,
                ascending: false,
            }) => AlbumSortMethod::TitleDesc,
            Some(TableSort {
                column: AlbumColumn::Artist,
                ascending: true,
            }) => AlbumSortMethod::ArtistAsc,
            Some(TableSort {
                column: AlbumColumn::Artist,
                ascending: false,
            }) => AlbumSortMethod::ArtistDesc,
            Some(TableSort {
                column: AlbumColumn::Date,
                ascending: true,
            }) => AlbumSortMethod::ReleaseAsc,
            Some(TableSort {
                column: AlbumColumn::Date,
                ascending: false,
            }) => AlbumSortMethod::ReleaseDesc,
            Some(TableSort {
                column: AlbumColumn::Label,
                ascending: true,
            }) => AlbumSortMethod::LabelAsc,
            Some(TableSort {
                column: AlbumColumn::Label,
                ascending: false,
            }) => AlbumSortMethod::LabelDesc,
            Some(TableSort {
                column: AlbumColumn::CatalogNumber,
                ascending: true,
            }) => AlbumSortMethod::CatalogAsc,
            Some(TableSort {
                column: AlbumColumn::CatalogNumber,
                ascending: false,
            }) => AlbumSortMethod::CatalogDesc,
            _ => AlbumSortMethod::ArtistAsc,
        };

        Ok(cx.list_albums(sort_method)?)
    }

    fn get_row(cx: &mut gpui::App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>> {
        Ok(cx.get_album_by_id(id.0 as i64, AlbumMethod::Metadata).ok())
    }

    fn get_column(&self, cx: &mut App, column: AlbumColumn) -> Option<SharedString> {
        match column {
            AlbumColumn::Title => Some(self.title.0.clone()),
            AlbumColumn::Artist => cx
                .get_artist_name_by_id(self.artist_id)
                .ok()
                .map(|v| (*v).clone().into()),
            AlbumColumn::Date => match self.date_precision {
                Some(1) => self.release_date.as_ref().and_then(|d| {
                    chrono::NaiveDate::parse_from_str(d.0.as_str(), "%Y-%m-%d")
                        .ok()
                        .map(|nd| nd.format("%x").to_string().into())
                }),
                Some(0) => self
                    .release_date
                    .as_ref()
                    .map(|d| d.0.as_str()[..4].to_string().into()),
                _ => None,
            },
            AlbumColumn::Label => self.label.as_ref().map(|v| v.0.clone()),
            AlbumColumn::CatalogNumber => self.catalog_number.as_ref().map(|v| v.0.clone()),
        }
    }

    fn get_image_path(&self) -> Option<SharedString> {
        Some(format!("!db://album/{}/thumb", self.id).into())
    }

    fn get_full_image_path(&self) -> Option<SharedString> {
        Some(format!("!db://album/{}/full", self.id).into())
    }

    fn has_images() -> bool {
        true
    }

    fn column_monospace(_column: AlbumColumn) -> bool {
        false
    }

    fn get_element_id(&self) -> impl Into<gpui::ElementId> {
        ("album", self.id as u32)
    }

    fn get_table_id(&self) -> Self::Identifier {
        (self.id as u32, self.title.0.clone().into())
    }

    fn default_columns() -> IndexMap<AlbumColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<AlbumColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(AlbumColumn::Title, 300.0);
        columns.insert(AlbumColumn::Artist, 200.0);
        columns.insert(AlbumColumn::Date, 100.0);
        columns.insert(AlbumColumn::Label, 150.0);
        // length is weird because the image column is 47.0
        columns.insert(AlbumColumn::CatalogNumber, 203.0);
        columns
    }

    fn get_drag_data(&self) -> Option<TableDragData> {
        Some(TableDragData::Album(AlbumDragData::new(
            self.id,
            self.title.0.clone(),
        )))
    }

    fn get_context_menu(
        &self,
        _cx: &mut App,
        context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<gpui::AnyElement> {
        Some(album_menu_for_table(self, context))
    }

    fn supports_grid_view() -> bool {
        true
    }

    fn get_grid_content(&self, cx: &mut App) -> Option<(SharedString, Option<SharedString>)> {
        let title = self.title.0.clone();
        let artist = cx
            .get_artist_name_by_id(self.artist_id)
            .ok()
            .map(|v| (*v).clone().into());
        Some((title, artist))
    }

    fn get_grid_content_for(
        &self,
        cx: &mut App,
        context: GridContext,
    ) -> Option<(SharedString, Option<SharedString>)> {
        let title = self.title.0.clone();

        let artist_part: Option<String> = match context {
            GridContext::Table => cx
                .get_artist_name_by_id(self.artist_id)
                .ok()
                .map(|v| (*v).to_string()),
            GridContext::Standalone => None,
        };

        let year_part: Option<String> = self
            .release_date
            .as_ref()
            .map(|d| d.0.as_str()[..4].to_string());

        let secondary = match (artist_part, year_part) {
            (Some(a), Some(y)) => Some(format!("{} • {}", a, y).into()),
            (Some(a), None) => Some(SharedString::from(a)),
            (None, Some(y)) => Some(SharedString::from(y)),
            (None, None) => None,
        };

        Some((title, secondary))
    }

    fn is_available(&self, cx: &mut App) -> bool {
        album_has_available_tracks(cx, self.id)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TrackColumn {
    TrackNumber,
    Title,
    Album,
    Artist,
    Length,
}

impl Column for TrackColumn {
    fn get_column_name(&self) -> SharedString {
        match self {
            TrackColumn::TrackNumber => tr!("TRACK_NUMBER", "#").into(),
            TrackColumn::Title => tr!("COLUMN_TITLE").into(),
            TrackColumn::Album => tr!("COLUMN_ALBUM", "Album").into(),
            TrackColumn::Artist => tr!("COLUMN_ARTIST").into(),
            TrackColumn::Length => tr!("COLUMN_LENGTH", "Length").into(),
        }
    }

    fn is_hideable(&self) -> bool {
        !matches!(self, TrackColumn::Title)
    }

    fn all_columns() -> &'static [Self] {
        &[
            TrackColumn::TrackNumber,
            TrackColumn::Title,
            TrackColumn::Album,
            TrackColumn::Artist,
            TrackColumn::Length,
        ]
    }
}

impl TableData<TrackColumn> for Track {
    type Identifier = (i64, String, Option<i64>, String);
    type ContextMenuContext = TrackContextMenuContext;

    fn get_table_name() -> SharedString {
        tr!("TABLE_TRACKS", "Tracks").into()
    }

    fn get_rows(
        cx: &mut gpui::App,
        sort: Option<TableSort<TrackColumn>>,
    ) -> anyhow::Result<Vec<Self::Identifier>> {
        let sort_method = match sort {
            Some(TableSort {
                column: TrackColumn::Title,
                ascending: true,
            }) => TrackSortMethod::TitleAsc,
            Some(TableSort {
                column: TrackColumn::Title,
                ascending: false,
            }) => TrackSortMethod::TitleDesc,
            Some(TableSort {
                column: TrackColumn::Artist,
                ascending: true,
            }) => TrackSortMethod::ArtistAsc,
            Some(TableSort {
                column: TrackColumn::Artist,
                ascending: false,
            }) => TrackSortMethod::ArtistDesc,
            Some(TableSort {
                column: TrackColumn::Album,
                ascending: true,
            }) => TrackSortMethod::AlbumAsc,
            Some(TableSort {
                column: TrackColumn::Album,
                ascending: false,
            }) => TrackSortMethod::AlbumDesc,
            Some(TableSort {
                column: TrackColumn::Length,
                ascending: true,
            }) => TrackSortMethod::DurationAsc,
            Some(TableSort {
                column: TrackColumn::Length,
                ascending: false,
            }) => TrackSortMethod::DurationDesc,
            Some(TableSort {
                column: TrackColumn::TrackNumber,
                ascending: true,
            }) => TrackSortMethod::TrackNumberAsc,
            Some(TableSort {
                column: TrackColumn::TrackNumber,
                ascending: false,
            }) => TrackSortMethod::TrackNumberDesc,
            _ => TrackSortMethod::ArtistAsc,
        };

        Ok(cx.list_tracks(sort_method)?)
    }

    fn get_row(cx: &mut gpui::App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>> {
        Ok(cx.get_track_by_id(id.0).ok())
    }

    fn get_column(&self, cx: &mut App, column: TrackColumn) -> Option<SharedString> {
        match column {
            TrackColumn::TrackNumber => {
                let vinyl_numbering = self
                    .album_id
                    .and_then(|id| cx.get_album_by_id(id, AlbumMethod::Metadata).ok())
                    .map(|album| album.vinyl_numbering)
                    .unwrap_or(false);

                match (self.disc_number, self.track_number) {
                    (Some(disc), Some(track)) => {
                        if vinyl_numbering {
                            let side = (b'A' + (disc - 1) as u8) as char;
                            Some(format!("{}{}", side, track).into())
                        } else {
                            Some(format!("{}-{}", disc, track).into())
                        }
                    }
                    (None, Some(track)) => Some(track.to_string().into()),
                    _ => None,
                }
            }
            TrackColumn::Title => Some(self.title.0.clone()),
            TrackColumn::Album => {
                if let Some(album_id) = self.album_id {
                    cx.get_album_by_id(album_id, AlbumMethod::Metadata)
                        .ok()
                        .map(|v| v.title.0.clone())
                } else {
                    None
                }
            }
            TrackColumn::Artist => {
                if let Some(artist) = &self.artist_names {
                    Some(artist.0.clone())
                } else if let Some(album_id) = self.album_id {
                    cx.get_album_by_id(album_id, AlbumMethod::Metadata)
                        .ok()
                        .and_then(|album| {
                            cx.get_artist_name_by_id(album.artist_id)
                                .ok()
                                .map(|v| (*v).clone().into())
                        })
                } else {
                    None
                }
            }
            TrackColumn::Length => {
                let minutes = self.duration / 60;
                let seconds = self.duration % 60;
                Some(format!("{:02}:{:02}", minutes, seconds).into())
            }
        }
    }

    fn get_image_path(&self) -> Option<SharedString> {
        None
    }

    fn get_full_image_path(&self) -> Option<SharedString> {
        None
    }

    fn has_images() -> bool {
        false
    }

    fn column_monospace(_column: TrackColumn) -> bool {
        false
    }

    fn get_element_id(&self) -> impl Into<gpui::ElementId> {
        ("track", self.id as u32)
    }

    fn get_table_id(&self) -> Self::Identifier {
        (
            self.id,
            self.title.0.clone().into(),
            self.album_id,
            self.location.to_string_lossy().to_string(),
        )
    }

    fn default_columns() -> IndexMap<TrackColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<TrackColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(TrackColumn::TrackNumber, 75.0);
        columns.insert(TrackColumn::Title, 350.0);
        columns.insert(TrackColumn::Album, 250.0);
        columns.insert(TrackColumn::Artist, 225.0);
        columns.insert(TrackColumn::Length, 100.0);
        columns
    }

    fn get_drag_data(&self) -> Option<TableDragData> {
        Some(TableDragData::Track(TrackDragData::from_track(
            self.id,
            self.album_id,
            self.location.clone(),
            self.title.0.clone(),
        )))
    }

    fn is_available(&self, _cx: &mut App) -> bool {
        is_track_available(self)
    }

    fn get_context_menu(
        &self,
        _cx: &mut App,
        context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<gpui::AnyElement> {
        Some(track_menu_for_table(
            self,
            is_track_available(self),
            context,
        ))
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ArtistColumn {
    Name,
    Albums,
    Tracks,
}

impl Column for ArtistColumn {
    fn get_column_name(&self) -> SharedString {
        match self {
            ArtistColumn::Name => tr!("COLUMN_NAME", "Name").into(),
            ArtistColumn::Albums => tr!("COLUMN_ALBUMS", "# of Albums").into(),
            ArtistColumn::Tracks => tr!("COLUMN_TRACKS", "# of Tracks").into(),
        }
    }

    fn is_hideable(&self) -> bool {
        !matches!(self, ArtistColumn::Name)
    }

    fn all_columns() -> &'static [Self] {
        &[
            ArtistColumn::Name,
            ArtistColumn::Albums,
            ArtistColumn::Tracks,
        ]
    }
}

impl TableData<ArtistColumn> for ArtistWithCounts {
    type Identifier = i64;
    type ContextMenuContext = ();

    fn get_table_name() -> SharedString {
        tr!("TABLE_ARTISTS", "Artists").into()
    }

    fn get_rows(
        cx: &mut gpui::App,
        sort: Option<TableSort<ArtistColumn>>,
    ) -> anyhow::Result<Vec<Self::Identifier>> {
        let sort_method = match sort {
            Some(TableSort {
                column: ArtistColumn::Name,
                ascending: true,
            }) => ArtistSortMethod::NameAsc,
            Some(TableSort {
                column: ArtistColumn::Name,
                ascending: false,
            }) => ArtistSortMethod::NameDesc,
            Some(TableSort {
                column: ArtistColumn::Albums,
                ascending: true,
            }) => ArtistSortMethod::AlbumsAsc,
            Some(TableSort {
                column: ArtistColumn::Albums,
                ascending: false,
            }) => ArtistSortMethod::AlbumsDesc,
            Some(TableSort {
                column: ArtistColumn::Tracks,
                ascending: true,
            }) => ArtistSortMethod::TracksAsc,
            Some(TableSort {
                column: ArtistColumn::Tracks,
                ascending: false,
            }) => ArtistSortMethod::TracksDesc,
            _ => ArtistSortMethod::NameAsc,
        };

        Ok(cx.list_artists(sort_method)?)
    }

    fn get_row(cx: &mut gpui::App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>> {
        Ok(cx.get_artist_with_counts(id).ok())
    }

    fn get_column(&self, _cx: &mut App, column: ArtistColumn) -> Option<SharedString> {
        match column {
            ArtistColumn::Name => self.name.as_ref().map(|v| v.0.clone()),
            ArtistColumn::Albums => Some(self.album_count.to_string().into()),
            ArtistColumn::Tracks => Some(self.track_count.to_string().into()),
        }
    }

    fn get_image_path(&self) -> Option<SharedString> {
        None
    }

    fn get_full_image_path(&self) -> Option<SharedString> {
        None
    }

    fn has_images() -> bool {
        false
    }

    fn column_monospace(_column: ArtistColumn) -> bool {
        false
    }

    fn get_element_id(&self) -> impl Into<gpui::ElementId> {
        ("artist", self.id as u32)
    }

    fn get_table_id(&self) -> Self::Identifier {
        self.id
    }

    fn is_available(&self, cx: &mut App) -> bool {
        artist_has_available_tracks(cx, self.id)
    }

    fn default_columns() -> IndexMap<ArtistColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<ArtistColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(ArtistColumn::Name, 400.0);
        columns.insert(ArtistColumn::Albums, 150.0);
        columns.insert(ArtistColumn::Tracks, 150.0);
        columns
    }
}
