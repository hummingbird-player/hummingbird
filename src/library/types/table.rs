use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use cntp_i18n::{Date, I18N_MANAGER, ListFunction, StringModifier, tr};
use gpui::{App, SharedString};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use super::{
    Album, ArtistWithCounts, DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR,
    DATE_PRECISION_YEAR_MONTH, DBString, Track,
};
pub use crate::library::db::AlbumColumn;
use crate::{
    library::db::{ArtistSortMethod, LibraryAccess, SortDirection, TrackSortMethod, albums},
    media::numbering::format_track_table_position,
    ui::{
        app::Pool,
        availability::{
            album_has_available_tracks, artist_has_available_tracks, is_track_available,
        },
        components::{
            drag_drop::{AlbumDragData, TrackDragData},
            managed_image::ManagedImageKey,
            table::table_data::{Column, GridContext, TableData, TableDragData, TableSort},
        },
        library::context_menus::{
            AlbumContextMenuContext, TrackContextMenuContext, album_menu_for_table,
            play_album_next, play_track_next, track_menu_for_table,
        },
        util::format_duration,
    },
};

fn parse_album_release_date(release_date: &DBString) -> Option<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(release_date.0.as_ref(), "%Y-%m-%d").ok()?;
    Some(DateTime::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0)?,
        Utc,
    ))
}

fn album_release_date_format(precision: i32) -> Option<(&'static str, &'static str)> {
    match precision {
        DATE_PRECISION_YEAR => Some(("Y", "medium")),
        DATE_PRECISION_YEAR_MONTH => Some(("YM", "medium")),
        DATE_PRECISION_FULL_DATE => Some(("YMD", "medium")),
        _ => None,
    }
}

fn format_album_release_date_with(
    release_date: Option<&DBString>,
    format: &'static str,
    length: &'static str,
) -> Option<SharedString> {
    let release_date = parse_album_release_date(release_date?)?;
    let format_var = (None, format);
    let length_var = (Some("length"), length);
    let variables = [&format_var, &length_var];
    let locale = &I18N_MANAGER.read().unwrap().locale;
    Some(Date.transform(locale, &release_date, &variables).into())
}

fn format_album_release_date(
    release_date: Option<&DBString>,
    date_precision: Option<i32>,
) -> Option<SharedString> {
    let (format, length) = album_release_date_format(date_precision?)?;
    format_album_release_date_with(release_date, format, length)
}

fn format_genres(genres: &[DBString]) -> Option<SharedString> {
    if genres.is_empty() {
        return None;
    }

    let genres: Vec<String> = genres.iter().map(|genre| genre.0.to_string()).collect();
    let manager = I18N_MANAGER.read().unwrap();
    Some(
        manager
            .locale
            .build_list(&genres)
            .with_list_function(ListFunction::Unit)
            .build()
            .into(),
    )
}

impl Column for AlbumColumn {
    fn get_column_name(&self) -> SharedString {
        match self {
            AlbumColumn::Title => tr!("COLUMN_TITLE", "Title").into(),
            AlbumColumn::Artist => tr!("COLUMN_ARTIST", "Artist").into(),
            AlbumColumn::Genres => tr!("COLUMN_GENRES", "Genres").into(),
            AlbumColumn::ReleaseDate => tr!("COLUMN_DATE", "Date").into(),
            AlbumColumn::Label => tr!("COLUMN_LABEL", "Label").into(),
            AlbumColumn::CatalogNumber => tr!("COLUMN_CATALOG_NUMBER", "Catalog Number").into(),
        }
    }

    fn is_hideable(&self) -> bool {
        !matches!(self, AlbumColumn::Title)
    }
}

impl TableData<AlbumColumn> for Album {
    type Identifier = u32;
    type ContextMenuContext = AlbumContextMenuContext;

    fn get_table_name() -> SharedString {
        tr!("TABLE_ALBUMS", "Albums").into()
    }

    fn get_rows(
        cx: &mut gpui::App,
        sort: Option<TableSort<AlbumColumn>>,
    ) -> anyhow::Result<Vec<Self::Identifier>> {
        let sort = sort.unwrap_or(TableSort {
            column: AlbumColumn::Artist,
            direction: SortDirection::Ascending,
        });
        let pool: &Pool = cx.global();
        Ok(crate::RUNTIME
            .block_on(
                albums()
                    .sort(sort.column, sort.direction)
                    .fetch_ids(&pool.0),
            )?
            .into_iter()
            .map(|id| id as u32)
            .collect())
    }

    fn get_row(cx: &mut gpui::App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>> {
        Ok(cx.get_album_by_id(id as i64).ok())
    }

    fn get_column(&self, _cx: &mut App, column: AlbumColumn) -> Option<SharedString> {
        match column {
            AlbumColumn::Title => Some(self.title.0.clone()),
            AlbumColumn::Artist => self.artist_display_override.as_ref().map(|v| v.0.clone()),
            AlbumColumn::Genres => format_genres(&self.genres),
            AlbumColumn::ReleaseDate => {
                format_album_release_date(self.release_date.as_ref(), self.date_precision)
            }
            AlbumColumn::Label => self.label.as_ref().map(|v| v.0.clone()),
            AlbumColumn::CatalogNumber => self.catalog_number.as_ref().map(|v| v.0.clone()),
        }
    }

    fn get_full_image_key(&self) -> Option<ManagedImageKey> {
        Some(ManagedImageKey::Album(self.id))
    }

    fn has_images() -> bool {
        true
    }

    fn get_element_id(&self) -> impl Into<gpui::ElementId> {
        ("album", self.id as u32)
    }

    fn get_table_id(&self) -> Self::Identifier {
        self.id as u32
    }

    fn available_columns() -> IndexMap<AlbumColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<AlbumColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(AlbumColumn::Title, 300.0);
        columns.insert(AlbumColumn::Artist, 200.0);
        columns.insert(AlbumColumn::Genres, 200.0);
        columns.insert(AlbumColumn::ReleaseDate, 125.0);
        columns.insert(AlbumColumn::Label, 150.0);
        // length is weird because the image column is 47.0
        columns.insert(AlbumColumn::CatalogNumber, 178.0);
        columns
    }

    fn default_columns() -> IndexMap<AlbumColumn, f32, FxBuildHasher> {
        let mut columns = Self::available_columns();
        columns.shift_remove(&AlbumColumn::Genres);
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
        window: &mut gpui::Window,
        cx: &mut App,
        context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<(gpui::AnyElement, Option<gpui::AnyElement>)> {
        Some(album_menu_for_table(self, context, window, cx))
    }

    fn handle_middle_mouse(
        &self,
        _window: &mut gpui::Window,
        cx: &mut App,
        _grid_context: GridContext,
    ) {
        play_album_next(cx, self);
    }

    fn supports_grid_view() -> bool {
        true
    }

    fn get_grid_content(&self, _cx: &mut App) -> Option<(SharedString, Option<SharedString>)> {
        let title = self.title.0.clone();
        let artist = self.artist_display_override.as_ref().map(|v| v.0.clone());
        Some((title, artist))
    }

    fn get_grid_content_for(
        &self,
        _cx: &mut App,
        context: GridContext,
    ) -> Option<(SharedString, Option<SharedString>)> {
        let title = self.title.0.clone();

        let artist_part: Option<String> = match context {
            GridContext::Table => self
                .artist_display_override
                .as_ref()
                .map(|v| v.0.to_string()),
            GridContext::Standalone => None,
        };

        let secondary = match artist_part {
            Some(artist) => {
                format_album_release_date_with(self.release_date.as_ref(), "Y", "medium")
                    .map(|year| format!("{artist} • {year}").into())
                    .or(Some(SharedString::from(artist)))
            }
            None => format_album_release_date(self.release_date.as_ref(), self.date_precision),
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
    Genres,
    Length,
}

impl Column for TrackColumn {
    fn get_column_name(&self) -> SharedString {
        match self {
            TrackColumn::TrackNumber => tr!("TRACK_NUMBER", "#").into(),
            TrackColumn::Title => tr!("COLUMN_TITLE").into(),
            TrackColumn::Album => tr!("COLUMN_ALBUM", "Album").into(),
            TrackColumn::Artist => tr!("COLUMN_ARTIST").into(),
            TrackColumn::Genres => tr!("COLUMN_GENRES").into(),
            TrackColumn::Length => tr!("COLUMN_LENGTH", "Length").into(),
        }
    }

    fn is_hideable(&self) -> bool {
        !matches!(self, TrackColumn::Title)
    }
}

pub fn track_table_sort(sort: Option<TableSort<TrackColumn>>) -> TrackSortMethod {
    match sort {
        Some(TableSort {
            column: TrackColumn::Title,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::TitleAsc,
        Some(TableSort {
            column: TrackColumn::Title,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::TitleDesc,
        Some(TableSort {
            column: TrackColumn::Artist,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::ArtistAsc,
        Some(TableSort {
            column: TrackColumn::Artist,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::ArtistDesc,
        Some(TableSort {
            column: TrackColumn::Album,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::AlbumAsc,
        Some(TableSort {
            column: TrackColumn::Album,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::AlbumDesc,
        Some(TableSort {
            column: TrackColumn::Length,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::DurationAsc,
        Some(TableSort {
            column: TrackColumn::Length,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::DurationDesc,
        Some(TableSort {
            column: TrackColumn::TrackNumber,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::TrackNumberAsc,
        Some(TableSort {
            column: TrackColumn::TrackNumber,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::TrackNumberDesc,
        Some(TableSort {
            column: TrackColumn::Genres,
            direction: SortDirection::Ascending,
        }) => TrackSortMethod::GenresAsc,
        Some(TableSort {
            column: TrackColumn::Genres,
            direction: SortDirection::Descending,
        }) => TrackSortMethod::GenresDesc,
        _ => TrackSortMethod::ArtistAsc,
    }
}

impl TableData<TrackColumn> for Track {
    type Identifier = i64;
    type ContextMenuContext = TrackContextMenuContext;

    fn get_table_name() -> SharedString {
        tr!("TABLE_TRACKS", "Tracks").into()
    }

    fn get_rows(
        cx: &mut gpui::App,
        sort: Option<TableSort<TrackColumn>>,
    ) -> anyhow::Result<Vec<Self::Identifier>> {
        Ok(cx
            .list_tracks(track_table_sort(sort))?
            .into_iter()
            .map(|(id, _, _, _)| id)
            .collect())
    }

    fn get_row(cx: &mut gpui::App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>> {
        Ok(cx.get_track_by_id(id).ok())
    }

    fn get_column(&self, cx: &mut App, column: TrackColumn) -> Option<SharedString> {
        match column {
            TrackColumn::TrackNumber => {
                let number_display_mode = self
                    .album_id
                    .and_then(|id| cx.get_album_by_id(id).ok())
                    .map(|album| album.number_display_mode)
                    .unwrap_or_default();

                format_track_table_position(
                    number_display_mode,
                    self.disc_number,
                    self.track_number,
                    self.track_section,
                )
                .map(SharedString::from)
            }
            TrackColumn::Title => Some(self.title.0.clone()),
            TrackColumn::Album => {
                if let Some(album_id) = self.album_id {
                    cx.get_album_by_id(album_id).ok().map(|v| v.title.0.clone())
                } else {
                    None
                }
            }
            TrackColumn::Artist => {
                if let Some(artist) = &self.artist_names {
                    Some(artist.0.clone())
                } else if let Some(album_id) = self.album_id {
                    cx.get_album_by_id(album_id).ok().and_then(|album| {
                        album.artist_display_override.as_ref().map(|v| v.0.clone())
                    })
                } else {
                    None
                }
            }
            TrackColumn::Genres => format_genres(&self.genres),
            TrackColumn::Length => Some(format_duration(self.duration, true).into()),
        }
    }

    fn get_full_image_key(&self) -> Option<ManagedImageKey> {
        Some(ManagedImageKey::Track(self.id))
    }

    fn has_images() -> bool {
        true
    }

    fn get_element_id(&self) -> impl Into<gpui::ElementId> {
        ("track", self.id as u32)
    }

    fn get_table_id(&self) -> Self::Identifier {
        self.id
    }

    fn available_columns() -> IndexMap<TrackColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<TrackColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(TrackColumn::TrackNumber, 75.0);
        columns.insert(TrackColumn::Title, 350.0);
        columns.insert(TrackColumn::Album, 250.0);
        columns.insert(TrackColumn::Artist, 225.0);
        columns.insert(TrackColumn::Genres, 225.0);
        columns.insert(TrackColumn::Length, 100.0);
        columns
    }

    fn default_columns() -> IndexMap<TrackColumn, f32, FxBuildHasher> {
        let mut columns = Self::available_columns();
        columns.shift_remove(&TrackColumn::Genres);
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

    fn is_available(&self, cx: &mut App) -> bool {
        is_track_available(cx, self)
    }

    fn get_context_menu(
        &self,
        window: &mut gpui::Window,
        cx: &mut App,
        context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<(gpui::AnyElement, Option<gpui::AnyElement>)> {
        Some(track_menu_for_table(
            self,
            is_track_available(cx, self),
            context,
            window,
            cx,
        ))
    }

    fn handle_middle_mouse(
        &self,
        _window: &mut gpui::Window,
        cx: &mut App,
        _grid_context: GridContext,
    ) {
        play_track_next(cx, self);
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
}

fn artist_table_sort(sort: Option<TableSort<ArtistColumn>>) -> ArtistSortMethod {
    match sort {
        Some(TableSort {
            column: ArtistColumn::Name,
            direction: SortDirection::Ascending,
        }) => ArtistSortMethod::NameAsc,
        Some(TableSort {
            column: ArtistColumn::Name,
            direction: SortDirection::Descending,
        }) => ArtistSortMethod::NameDesc,
        Some(TableSort {
            column: ArtistColumn::Albums,
            direction: SortDirection::Ascending,
        }) => ArtistSortMethod::AlbumsAsc,
        Some(TableSort {
            column: ArtistColumn::Albums,
            direction: SortDirection::Descending,
        }) => ArtistSortMethod::AlbumsDesc,
        Some(TableSort {
            column: ArtistColumn::Tracks,
            direction: SortDirection::Ascending,
        }) => ArtistSortMethod::TracksAsc,
        Some(TableSort {
            column: ArtistColumn::Tracks,
            direction: SortDirection::Descending,
        }) => ArtistSortMethod::TracksDesc,
        None => ArtistSortMethod::NameAsc,
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
        Ok(cx.list_artists(artist_table_sort(sort))?)
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

    fn get_full_image_key(&self) -> Option<ManagedImageKey> {
        None
    }

    fn has_images() -> bool {
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

    fn available_columns() -> IndexMap<ArtistColumn, f32, FxBuildHasher> {
        let s = FxBuildHasher;
        let mut columns: IndexMap<ArtistColumn, f32, FxBuildHasher> = IndexMap::with_hasher(s);
        columns.insert(ArtistColumn::Name, 400.0);
        columns.insert(ArtistColumn::Albums, 150.0);
        columns.insert(ArtistColumn::Tracks, 150.0);
        columns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::types::{
        DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH, DBString,
    };
    use chrono::{TimeZone, Utc};

    fn sort<C: Column>(column: C, direction: SortDirection) -> Option<TableSort<C>> {
        Some(TableSort { column, direction })
    }

    #[test]
    fn track_table_sort_maps_every_column_and_direction() {
        let cases = [
            (
                sort(TrackColumn::Title, SortDirection::Ascending),
                TrackSortMethod::TitleAsc,
            ),
            (
                sort(TrackColumn::Title, SortDirection::Descending),
                TrackSortMethod::TitleDesc,
            ),
            (
                sort(TrackColumn::Artist, SortDirection::Ascending),
                TrackSortMethod::ArtistAsc,
            ),
            (
                sort(TrackColumn::Artist, SortDirection::Descending),
                TrackSortMethod::ArtistDesc,
            ),
            (
                sort(TrackColumn::Album, SortDirection::Ascending),
                TrackSortMethod::AlbumAsc,
            ),
            (
                sort(TrackColumn::Album, SortDirection::Descending),
                TrackSortMethod::AlbumDesc,
            ),
            (
                sort(TrackColumn::Length, SortDirection::Ascending),
                TrackSortMethod::DurationAsc,
            ),
            (
                sort(TrackColumn::Length, SortDirection::Descending),
                TrackSortMethod::DurationDesc,
            ),
            (
                sort(TrackColumn::TrackNumber, SortDirection::Ascending),
                TrackSortMethod::TrackNumberAsc,
            ),
            (
                sort(TrackColumn::TrackNumber, SortDirection::Descending),
                TrackSortMethod::TrackNumberDesc,
            ),
            (
                sort(TrackColumn::Genres, SortDirection::Ascending),
                TrackSortMethod::GenresAsc,
            ),
            (
                sort(TrackColumn::Genres, SortDirection::Descending),
                TrackSortMethod::GenresDesc,
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(track_table_sort(input), expected);
        }
        assert_eq!(track_table_sort(None), TrackSortMethod::ArtistAsc);
    }

    #[test]
    fn artist_table_sort_maps_every_column_and_direction() {
        let cases = [
            (
                sort(ArtistColumn::Name, SortDirection::Ascending),
                ArtistSortMethod::NameAsc,
            ),
            (
                sort(ArtistColumn::Name, SortDirection::Descending),
                ArtistSortMethod::NameDesc,
            ),
            (
                sort(ArtistColumn::Albums, SortDirection::Ascending),
                ArtistSortMethod::AlbumsAsc,
            ),
            (
                sort(ArtistColumn::Albums, SortDirection::Descending),
                ArtistSortMethod::AlbumsDesc,
            ),
            (
                sort(ArtistColumn::Tracks, SortDirection::Ascending),
                ArtistSortMethod::TracksAsc,
            ),
            (
                sort(ArtistColumn::Tracks, SortDirection::Descending),
                ArtistSortMethod::TracksDesc,
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(artist_table_sort(input), expected);
        }
        assert_eq!(artist_table_sort(None), ArtistSortMethod::NameAsc);
    }

    #[test]
    fn selects_release_date_formats_for_each_precision() {
        assert_eq!(
            album_release_date_format(DATE_PRECISION_YEAR),
            Some(("Y", "medium"))
        );
        assert_eq!(
            album_release_date_format(DATE_PRECISION_YEAR_MONTH),
            Some(("YM", "medium"))
        );
        assert_eq!(
            album_release_date_format(DATE_PRECISION_FULL_DATE),
            Some(("YMD", "medium"))
        );
    }

    #[test]
    fn parses_stored_release_dates_at_utc_midnight() {
        assert_eq!(
            parse_album_release_date(&DBString::from("1995-06-01")),
            Some(Utc.with_ymd_and_hms(1995, 6, 1, 0, 0, 0).single().unwrap())
        );
    }
}
