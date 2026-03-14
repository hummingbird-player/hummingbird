use std::{fmt::Debug, hash::Hash, sync::Arc};

use gpui::{AnyElement, App, ElementId, SharedString};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use crate::ui::components::drag_drop::{AlbumDragData, TrackDragData};

#[derive(Clone, Debug)]
pub enum TableDragData {
    Track(TrackDragData),
    Album(AlbumDragData),
}

// table layout constants
pub const TABLE_MAX_WIDTH: f32 = 1000.0;
pub const TABLE_IMAGE_COLUMN_WIDTH: f32 = 47.0;

// column resize constants
pub const COLUMN_MIN_WIDTH: f32 = 50.0;
pub const COLUMN_RESIZE_HANDLE_WIDTH: f32 = 6.0;
pub const TABLE_HEADER_GROUP: &str = "table-header-group";

pub trait Column: Clone + Copy + Debug + Hash + PartialEq + Eq {
    /// Retrieves the friendly name text of the column.
    fn get_column_name(&self) -> SharedString;

    /// Returns whether this column can be resized by the user.
    /// Defaults to true.
    fn is_resizable(&self) -> bool {
        true
    }

    /// Returns whether this column can be hidden by the user.
    /// Return `false` for essential columns like "Title".
    /// Defaults to true.
    fn is_hideable(&self) -> bool {
        true
    }

    /// Returns all possible column variants for this type.
    /// Required for building the column visibility menu.
    fn all_columns() -> &'static [Self];
}

#[derive(Copy, Clone)]
pub struct TableSort<C>
where
    C: Column,
{
    pub column: C,
    pub ascending: bool,
}

/// Context in which a grid item is being displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridContext {
    /// Inside a Table component
    Table,
    /// Standalone / outside table
    Standalone,
}

/// The TableData trait defines the interface for retrieving, sorting, and listing data for a table.
/// Implementing this trait allows a table to display data in a structured manner.
pub trait TableData<C>: Sized
where
    C: Column,
{
    type Identifier: Clone + Debug;
    type ContextMenuContext: Clone;

    /// Retrieves the name of the table.
    fn get_table_name() -> SharedString;

    /// Retrieves the rows of the table. The rows are returned as a vector of identifiers, which
    /// can be used to retrieve the full row data. The sort parameter can be used to specify the
    /// sorting order of the rows.
    fn get_rows(cx: &mut App, sort: Option<TableSort<C>>) -> anyhow::Result<Vec<Self::Identifier>>;

    /// Retrieves a specific row of the table. The row is returned as an Arc to the table data,
    /// which can be used to retrieve the row data as SharedStrings. The id parameter is used to
    /// identify the row to retrieve.
    fn get_row(cx: &mut App, id: Self::Identifier) -> anyhow::Result<Option<Arc<Self>>>;

    /// Retrieves a column from the row.
    fn get_column(&self, cx: &mut App, column: C) -> Option<SharedString>;

    /// Returns true if the rows may contain images. This is used during the layout phase to
    /// determine if placeholder covers and the header section should be displayed.
    fn has_images() -> bool;

    /// Retrieves the associated image for the row.
    fn get_image_path(&self) -> Option<SharedString>;

    /// Retrieves the full-quality image for the row.
    fn get_full_image_path(&self) -> Option<SharedString>;

    /// Retrieves the default column widths for the table.
    fn default_columns() -> IndexMap<C, f32, FxBuildHasher>;

    /// Returns a boolean indicating whether or not a given column should be displayed using a
    /// monospaced font.
    ///
    /// This should be true for columns that contain mostly numbers, like a date or time.
    fn column_monospace(column: C) -> bool;

    /// Retrieves a unique element id for the row. This is different from the row id, as it is
    /// used to identify the row in GPUI.
    fn get_element_id(&self) -> impl Into<ElementId>;

    /// Retrieves the table ID for the row.
    fn get_table_id(&self) -> Self::Identifier;

    /// Returns whether the row is currently available for interaction.
    fn is_available(&self, _cx: &mut App) -> bool {
        true
    }

    /// Returns drag data for this row, if dragging is supported. If None is returned, dragging is
    /// not supported. Default implementation returns None.
    fn get_drag_data(&self) -> Option<TableDragData> {
        None
    }

    /// Returns the context menu for this row in the current display context.
    fn get_context_menu(
        &self,
        _cx: &mut App,
        _context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<AnyElement> {
        None
    }

    /// Returns true if the table supports rendering as a grid view.
    fn supports_grid_view() -> bool {
        false
    }

    /// Retrieves the content for the grid item relative to the table data.
    /// Returns a tuple of (Primary string, Optional Secondary string).
    fn get_grid_content(&self, _cx: &mut App) -> Option<(SharedString, Option<SharedString>)> {
        None
    }

    /// Retrieves the content for the grid item in a given context.
    /// Returns a tuple of (Primary string, Optional Secondary string).
    /// By default, delegates to `get_grid_content`.
    fn get_grid_content_for(
        &self,
        cx: &mut App,
        _context: GridContext,
    ) -> Option<(SharedString, Option<SharedString>)> {
        self.get_grid_content(cx)
    }
}
