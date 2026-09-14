use super::*;

pub(super) struct TableColumnHeader<T: TableData<C>, C: Column> {
    table: WeakEntity<Table<T, C>>,
}

impl<T: TableData<C>, C: Column> TableColumnHeader<T, C> {
    pub(super) fn new(
        cx: &mut App,
        table: WeakEntity<Table<T, C>>,
        columns: &Entity<Arc<IndexMap<C, f32, FxBuildHasher>>>,
        sort: &Entity<Option<TableSort<C>>>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            // reads alone do not invalidate a cached reader
            cx.observe(columns, |_, _, cx| cx.notify()).detach();
            cx.observe(sort, |_, _, cx| cx.notify()).detach();
            Self { table }
        })
    }
}

impl<T: TableData<C>, C: Column> Render for TableColumnHeader<T, C> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.table
            .update(cx, |table, cx| table.render_header(cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}
