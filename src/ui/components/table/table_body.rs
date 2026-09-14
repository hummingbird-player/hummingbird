use super::*;

pub(super) struct TableBody<T: TableData<C>, C: Column> {
    table: WeakEntity<Table<T, C>>,
}

impl<T: TableData<C>, C: Column> TableBody<T, C> {
    pub(super) fn new(
        cx: &mut App,
        table: WeakEntity<Table<T, C>>,
        columns: &Entity<Arc<IndexMap<C, f32, FxBuildHasher>>>,
        items: &ItemListResource<T, C>,
        view_mode: &Entity<TableViewMode>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(columns, |_, _, cx| cx.notify()).detach();
            cx.observe(items, |_, _, cx| cx.notify()).detach();
            cx.observe(view_mode, |_, _, cx| cx.notify()).detach();
            Self { table }
        })
    }
}

impl<T: TableData<C>, C: Column> Render for TableBody<T, C> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // rendering under this view makes vertical scrolling dirty only the body
        self.table
            .update(cx, |table, cx| table.render_body(cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}
