use std::{fmt::Debug, future::Future, hash::Hash, pin::Pin, sync::Arc};

use gpui::{AnyElement, App, SharedString, Window};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;
use sqlx::SqlitePool;

use crate::{
    library::db::SortDirection,
    ui::components::{
        drag_drop::{AlbumDragData, TrackDragData},
        managed_image::ManagedImageKey,
    },
};

#[derive(Clone, Debug)]
pub enum TableDragData {
    Track(TrackDragData),
    Album(AlbumDragData),
}

/// Drag payload for column header reordering.
#[derive(Clone, Debug)]
pub struct ColumnReorderDrag {
    pub source_index: usize,
}

// table layout constants
pub const TABLE_MAX_WIDTH: f32 = 1000.0;
pub const TABLE_IMAGE_COLUMN_WIDTH: f32 = 47.0;
pub const TABLE_HEADER_HEIGHT: f32 = 36.0;

// column resize constants
pub const COLUMN_MIN_WIDTH: f32 = 50.0;
pub const COLUMN_RESIZE_HANDLE_WIDTH: f32 = 6.0;
pub const TABLE_HEADER_GROUP: &str = "table-header-group";

pub trait Column: Clone + Copy + Debug + Hash + PartialEq + Eq + Send + Sync + 'static {
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
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TableSort<C>
where
    C: Column,
{
    pub column: C,
    pub direction: SortDirection,
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
pub type TableFuture<T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'static>>;

pub trait TableData<C>: Sized + Send + Sync + 'static
where
    C: Column,
{
    type Identifier: Clone + Debug + Send + Sync + 'static;
    type ContextMenuContext: Clone;
    type RowState: Clone + Default + Send + Sync + 'static;

    /// Retrieves the name of the table.
    fn get_table_name() -> SharedString;

    /// Builds the asynchronous query for the table's ordered row identifiers.
    ///
    /// Implementations should compose the typed query builder directly rather than delegate to
    /// another database-access wrapper.
    fn load_rows(
        pool: SqlitePool,
        sort: Option<TableSort<C>>,
    ) -> TableFuture<Vec<Self::Identifier>>;

    /// Builds the asynchronous query for one materialized row.
    ///
    /// `visible_columns` identifies any optional projections needed by the row. A replacement
    /// request is started when that projection changes.
    fn load_row(
        pool: SqlitePool,
        id: Self::Identifier,
        visible_columns: Vec<C>,
    ) -> TableFuture<Option<(Arc<Self>, Self::RowState)>>;

    /// Retrieves a column from the row.
    fn get_column(
        &self,
        cx: &mut App,
        column: C,
        row_state: &Self::RowState,
    ) -> Option<SharedString>;

    /// Returns true if the rows may contain images. This is used during the layout phase to
    /// determine if placeholder covers and the header section should be displayed.
    fn has_images() -> bool;

    /// Retrieves the full-quality key for the row, for use with `managed_image`.
    fn get_full_image_key(&self) -> Option<ManagedImageKey>;

    /// Retrieves every column supported by the table in its natural order and width.
    fn available_columns() -> IndexMap<C, f32, FxBuildHasher>;

    /// Retrieves the columns visible when the table has no saved settings.
    fn default_columns() -> IndexMap<C, f32, FxBuildHasher> {
        Self::available_columns()
    }

    /// Retrieves the table ID for the row.
    fn get_table_id(&self) -> Self::Identifier;

    /// Returns whether the row is currently available for interaction.
    fn is_available(&self, _cx: &mut App, _row_state: &Self::RowState) -> bool {
        true
    }

    /// Returns drag data for this row, if dragging is supported. If None is returned, dragging is
    /// not supported. Default implementation returns None.
    fn get_drag_data(&self) -> Option<TableDragData> {
        None
    }

    /// Returns the context menu for this row in the current display context.
    /// The first element is the menu content (rendered inside the context popup).
    /// The second element is an optional overlay (e.g. a modal) rendered outside
    /// the context popup so it is not nested inside `deferred`.
    fn get_context_menu(
        &self,
        _window: &mut Window,
        _cx: &mut App,
        _context: &Self::ContextMenuContext,
        _grid_context: GridContext,
    ) -> Option<(AnyElement, Option<AnyElement>)> {
        None
    }

    /// Optional middle mouse button handler for this row.
    fn handle_middle_mouse(&self, _window: &mut Window, _cx: &mut App, _grid_context: GridContext) {
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
