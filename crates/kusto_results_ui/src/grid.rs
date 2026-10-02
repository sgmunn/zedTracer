use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use editor::actions::Cancel;
use editor::{Editor, EditorEvent};
use gpui::{
    Anchor, AnyElement, Bounds, ClickEvent, ClipboardItem, Context, DefiniteLength, DismissEvent,
    DragMoveEvent, Empty, Entity, EventEmitter, FocusHandle, Focusable, Length, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Render, ScrollStrategy,
    SharedString, Subscription, Task, Window, actions, anchored, canvas, deferred, div, px,
};
use gpui_util::ResultExt as _;
use kusto_results::export::{copy_text, datatable, html, markdown};
use kusto_results::filter::ColumnFilter;
use kusto_results::view::{
    CellSelection, SortColumn, SortDirection, ViewState, display_column_order, selected_positions,
    severity_level, toggle_row, visible_rows,
};
use kusto_results::{ColumnLayout, ResultSet, TableView};
use ui::{
    Button, ColumnWidthConfig, ContextMenu, IconButton, IconName, IconSize, PopoverMenu,
    ResizableColumnsState, SpinnerLabel, Table, TableInteractionState, TableResizeBehavior,
    Tooltip, prelude::*,
};

use crate::filter_popover::{FilterChanged, FilterPopover};
use crate::results_settings::ResultsSettings;
use crate::row_details_panel::ActiveSelection;
use settings::Settings as _;

actions!(
    result_grid,
    [
        /// Copies the selection as tab-separated text, or the whole table when nothing is selected.
        Copy,
        /// Copies the selection as a Markdown table.
        CopyAsMarkdown,
        /// Copies the selection as HTML table markup.
        CopyAsHtml,
        /// Copies the selection as a KQL datatable expression.
        CopyAsDatatable,
        /// Moves the selected cell up.
        MoveUp,
        /// Moves the selected cell down.
        MoveDown,
        /// Moves the selected cell left.
        MoveLeft,
        /// Moves the selected cell right.
        MoveRight,
        /// Extends the selection up.
        ExtendUp,
        /// Extends the selection down.
        ExtendDown,
        /// Extends the selection left.
        ExtendLeft,
        /// Extends the selection right.
        ExtendRight,
        /// Moves the selected cell up by a screen of rows.
        PageUp,
        /// Moves the selected cell down by a screen of rows.
        PageDown,
        /// Selects the whole table.
        SelectAll,
        /// Shows or hides the search box.
        ToggleSearch,
        /// Removes every column filter.
        ClearAllFilters,
        /// Clears the selection.
        ClearSelection,
    ]
);

/// A first width for a column without a saved one: wide enough for its label and the start of
/// its values, and never wider than 500 px.
fn content_width(table: &kusto_results::Table, column: usize) -> Pixels {
    const SAMPLED_ROWS: usize = 200;
    const CHARACTER_WIDTH: f32 = 7.5;
    // Room for the sort and filter buttons in the header.
    const HEADER_CHROME: f32 = 64.;
    let label = table
        .columns
        .get(column)
        .map_or(0, |column| column.name.chars().count());
    let longest_value = table
        .rows
        .iter()
        .take(SAMPLED_ROWS)
        .filter_map(|row| row.get(column))
        .map(|cell| cell.display_text().chars().count())
        .max()
        .unwrap_or(0);
    let characters = label.max(longest_value) as f32;
    px((characters * CHARACTER_WIDTH + HEADER_CHROME).clamp(96., 500.))
}

/// Work that is still running after this long shows a busy indicator (GRD-9).
const BUSY_INDICATOR_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
/// How often a drag held beyond the top or bottom edge scrolls and extends the selection.
const AUTO_SCROLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// The header sits above the rows inside the table's bounds.
const HEADER_HEIGHT: Pixels = px(32.);

const GUTTER_WIDTH: Pixels = px(56.);
const DEFAULT_COLUMN_WIDTH: Pixels = px(160.);

#[derive(Debug, Clone, PartialEq)]
pub enum ResultGridEvent {
    /// The selected rows changed, as source row indexes in display order.
    SelectionChanged { rows: Vec<usize> },
    /// The user changed the order or widths of the columns. A saved result writes this back.
    LayoutChanged(TableView),
}

/// A header dragged to a new place among the columns.
#[derive(Clone)]
struct DraggedHeader {
    position: usize,
    label: SharedString,
}

impl Render for DraggedHeader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(cx.theme().colors().element_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .child(self.label.clone())
    }
}

/// The right edge of a header being dragged to resize its column.
#[derive(Clone)]
struct ResizeDrag {
    /// The column at this display position, or the row-number gutter when `None`.
    position: Option<usize>,
}

const MINIMUM_COLUMN_WIDTH: Pixels = px(48.);
const MINIMUM_GUTTER_WIDTH: Pixels = px(40.);

/// A results table: the rows of one table of a result set after search, filters and sort.
pub struct ResultGrid {
    result: Arc<ResultSet>,
    table_index: usize,
    view_state: ViewState,
    /// Original column indexes in the order they are displayed.
    column_order: Vec<usize>,
    visible_rows: Arc<Vec<usize>>,
    /// The rows the grid starts from, before search and filters; `None` is the whole table.
    scope: Option<Arc<Vec<usize>>>,
    /// The name the column layout is saved under.
    view_name: String,
    selection: Option<CellSelection>,
    /// Rows selected besides the rectangle, by position in the visible rows.
    added_rows: BTreeSet<usize>,
    /// A column edge is being dragged, so the layout is saved when the drag ends.
    layout_dirty: bool,
    /// Where the table, header included, was last laid out, to tell when a drag leaves it.
    table_bounds: Rc<Cell<Bounds<Pixels>>>,
    auto_scroll_task: Option<Task<()>>,
    dragging_selection: bool,
    /// Whether the drag in progress started on a row number, and so selects whole rows.
    dragging_rows: bool,
    interaction_state: Entity<TableInteractionState>,
    column_widths: Entity<ResizableColumnsState>,
    /// Bumped on every change to the view state; work for an older number is abandoned.
    generation: Arc<AtomicU64>,
    busy: bool,
    /// What the busy work is, for the indicator once it has run long enough to show.
    busy_action: &'static str,
    show_busy_indicator: bool,
    busy_indicator_task: Option<Task<()>>,
    search_open: bool,
    recompute_task: Option<Task<()>>,
    /// How many rows the last frame built, to show that only visible rows are created.
    last_rendered_rows: Cell<usize>,
    focus_handle: FocusHandle,
    search_editor: Entity<Editor>,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
    _search_subscription: Subscription,
}

/// What distinguishes one grid of a table from another.
#[derive(Default)]
pub struct GridOptions {
    /// The name the column layout is saved under, when it is not the table's own. The
    /// structured view of a table saves its layout apart from the ordinary grid's.
    pub view_name: Option<String>,
    /// The source rows the grid starts from, when it shows only some of the table, as the
    /// structured view shows one activity's events.
    pub scope: Option<Arc<Vec<usize>>>,
}

impl EventEmitter<ResultGridEvent> for ResultGrid {}

impl Focusable for ResultGrid {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ResultGrid {
    pub fn new(
        result: Arc<ResultSet>,
        table_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_options(result, table_index, GridOptions::default(), window, cx)
    }

    pub fn with_options(
        result: Arc<ResultSet>,
        table_index: usize,
        options: GridOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let table = result.tables.get(table_index);
        let table_rows = table.map_or(0, |table| table.rows.len());
        let column_count = table.map_or(0, |table| table.columns.len());
        let view_name = options
            .view_name
            .or_else(|| table.map(|table| table.name.clone()))
            .unwrap_or_default();
        let layout = result.table_view(&view_name);
        let column_order = display_column_order(column_count, layout);

        let saved_width = |index: usize| {
            layout
                .and_then(|layout| layout.columns.as_ref())
                .and_then(|columns| columns.iter().find(|saved| saved.index == index))
                .and_then(|saved| saved.width)
                .map(|width| px(width as f32))
        };
        let mut widths = vec![
            layout
                .and_then(|layout| layout.gutter_width)
                .map_or(GUTTER_WIDTH, |width| px(width as f32)),
        ];
        widths.extend(column_order.iter().map(|&index| {
            saved_width(index)
                .or_else(|| table.map(|table| content_width(table, index)))
                .unwrap_or(DEFAULT_COLUMN_WIDTH)
        }));
        let mut behavior = vec![TableResizeBehavior::None];
        behavior.extend(std::iter::repeat_n(
            TableResizeBehavior::MinSize(0.05),
            column_count,
        ));

        cx.on_release(|grid, cx| {
            let active = ActiveSelection::shared(cx);
            active.update(cx, |active, cx| {
                let owns_subject = active
                    .result
                    .as_ref()
                    .is_some_and(|result| Arc::ptr_eq(result, &grid.result));
                if owns_subject {
                    *active = ActiveSelection::default();
                    cx.notify();
                }
            });
        })
        .detach();

        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search", window, cx);
            editor
        });
        let search_subscription = cx.subscribe(&search_editor, |this, editor, event, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                let search = editor.read(cx).text(cx);
                this.set_search(search, cx);
            }
        });

        Self {
            result,
            table_index,
            view_state: ViewState::default(),
            column_order,
            visible_rows: options
                .scope
                .clone()
                .unwrap_or_else(|| Arc::new((0..table_rows).collect())),
            scope: options.scope,
            view_name,
            selection: None,
            added_rows: BTreeSet::new(),
            layout_dirty: false,
            table_bounds: Rc::new(Cell::new(Bounds::default())),
            auto_scroll_task: None,
            dragging_selection: false,
            dragging_rows: false,
            interaction_state: cx.new(|cx| TableInteractionState::new(cx)),
            column_widths: cx
                .new(|_| ResizableColumnsState::new(column_count + 1, widths, behavior)),
            generation: Arc::new(AtomicU64::new(0)),
            busy: false,
            busy_action: "Working on",
            show_busy_indicator: false,
            busy_indicator_task: None,
            search_open: false,
            recompute_task: None,
            last_rendered_rows: Cell::new(0),
            focus_handle: cx.focus_handle(),
            search_editor,
            context_menu: None,
            _search_subscription: search_subscription,
        }
    }

    pub fn table_name(&self) -> String {
        self.result
            .tables
            .get(self.table_index)
            .map_or_else(String::new, |table| table.name.clone())
    }

    pub fn last_rendered_rows(&self) -> usize {
        self.last_rendered_rows.get()
    }

    pub fn interaction_state(&self) -> &Entity<TableInteractionState> {
        &self.interaction_state
    }

    #[cfg(test)]
    pub(crate) fn visible_rows_for_test(&self) -> &[usize] {
        &self.visible_rows
    }

    pub fn visible_row_count(&self) -> usize {
        self.visible_rows.len()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Whether the busy indicator shows: work that has run for 250 ms and is not done.
    pub fn shows_busy_indicator(&self) -> bool {
        self.busy && self.show_busy_indicator
    }

    pub fn search_is_open(&self) -> bool {
        self.search_open
    }

    /// Shows the search box and focuses it, or hides it and clears the search, so that a
    /// search never filters rows invisibly.
    pub fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = !self.search_open;
        if self.search_open {
            window.focus(&self.search_editor.focus_handle(cx), cx);
        } else {
            self.search_editor
                .update(cx, |editor, cx| editor.clear(window, cx));
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    pub fn view_state(&self) -> &ViewState {
        &self.view_state
    }

    pub fn column_order(&self) -> &[usize] {
        &self.column_order
    }

    pub fn selection(&self) -> Option<CellSelection> {
        self.selection
    }

    /// The selected rows as source row indexes, in display order.
    pub fn selected_source_rows(&self) -> Vec<usize> {
        selected_positions(self.selection, &self.added_rows)
            .into_iter()
            .filter_map(|position| self.visible_rows.get(position).copied())
            .collect()
    }

    /// The footer text: how many rows show out of how many there are, and what is selected.
    pub fn status_text(&self) -> String {
        let total = self.scope_row_count();
        let shown = self.visible_rows.len();
        let mut parts = Vec::new();
        parts.push(if shown == total {
            format!("{total} rows")
        } else {
            format!("{shown} of {total} rows")
        });
        let selected = self.selected_source_rows();
        match selected.as_slice() {
            [] => {}
            [row] => parts.push(format!("Row {} selected", row + 1)),
            rows => parts.push(format!("{} rows selected", rows.len())),
        }
        parts.join(" · ")
    }

    /// Current column widths, gutter first.
    pub fn column_widths(&self, window: &Window, cx: &gpui::App) -> Vec<Pixels> {
        ColumnWidthConfig::Resizable(self.column_widths.clone())
            .widths_to_render(cx)
            .map(|widths| {
                widths
                    .as_slice()
                    .iter()
                    .map(|length| match length {
                        Length::Definite(DefiniteLength::Absolute(absolute)) => {
                            absolute.to_pixels(window.rem_size())
                        }
                        _ => px(0.),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A click on a column header: ascending, then descending, then the original order.
    pub fn click_sort(&mut self, column: usize, cx: &mut Context<Self>) {
        self.view_state.sort = self.view_state.sort.after_click(SortColumn::Column(column));
        self.recompute("Sorting", cx);
    }

    pub fn set_search(&mut self, search: String, cx: &mut Context<Self>) {
        self.view_state.search = search;
        self.recompute("Searching", cx);
    }

    /// Sets or removes the filter on a column, leaving the other columns' filters alone.
    pub fn set_filter(
        &mut self,
        column: usize,
        filter: Option<ColumnFilter>,
        cx: &mut Context<Self>,
    ) {
        match filter {
            Some(filter) => {
                self.view_state.filters.insert(column, filter);
            }
            None => {
                self.view_state.filters.remove(&column);
            }
        }
        self.recompute("Filtering", cx);
    }

    /// Copies the selection, or the whole table when nothing is selected, as tab-separated
    /// text. One selected cell copies as its bare value.
    pub fn copy_selection(&self, cx: &mut Context<Self>) {
        self.copy_as(copy_text, cx);
    }

    pub fn copy_as_markdown(&self, cx: &mut Context<Self>) {
        self.copy_as(markdown, cx);
    }

    pub fn copy_as_html(&self, cx: &mut Context<Self>) {
        self.copy_as(html, cx);
    }

    pub fn copy_as_datatable(&self, cx: &mut Context<Self>) {
        self.copy_as(datatable, cx);
    }

    fn copy_as(
        &self,
        format: fn(&kusto_results::Table, &[usize], &[usize]) -> String,
        cx: &mut Context<Self>,
    ) {
        let Some(table) = self.result.tables.get(self.table_index) else {
            return;
        };
        let (rows, columns) = match self.selection {
            Some(selection) => (
                self.selected_source_rows(),
                selection.source_columns(&self.column_order),
            ),
            None => (
                (0..table.rows.len()).collect(),
                (0..table.columns.len()).collect(),
            ),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(format(table, &rows, &columns)));
    }

    /// The order and widths of the columns as a saved view of this table.
    pub fn layout(&self, window: &Window, cx: &gpui::App) -> Option<TableView> {
        self.result.tables.get(self.table_index)?;
        let widths = self.column_widths(window, cx);
        let whole_pixels = |width: Pixels| f32::from(width).round() as u32;
        Some(TableView {
            name: self.view_name.clone(),
            gutter_width: widths.first().copied().map(whole_pixels),
            columns: Some(
                self.column_order
                    .iter()
                    .enumerate()
                    .map(|(position, &index)| ColumnLayout {
                        index,
                        width: widths.get(position + 1).copied().map(whole_pixels),
                    })
                    .collect(),
            ),
        })
    }

    fn emit_layout(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(layout) = self.layout(window, cx) {
            cx.emit(ResultGridEvent::LayoutChanged(layout));
        }
    }

    /// Sets the width of the row-number gutter so its right edge follows the pointer.
    fn resize_gutter(&mut self, width: Pixels, cx: &mut Context<Self>) {
        self.layout_dirty = true;
        self.column_widths.update(cx, |state, cx| {
            state.set_column_configuration(
                0,
                width.max(MINIMUM_GUTTER_WIDTH),
                TableResizeBehavior::None,
            );
            cx.notify();
        });
    }

    /// Sets the width of a column so its right edge follows the pointer. `pointer_x` and
    /// `table_left` are in window coordinates.
    fn resize_column(
        &mut self,
        position: usize,
        pointer_x: Pixels,
        table_left: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let widths = self.column_widths(window, cx);
        let scrolled = self
            .interaction_state
            .read(cx)
            .horizontal_scroll_handle
            .offset()
            .x;
        // Width index 0 is the pinned gutter, which does not scroll.
        let left_edge: Pixels = widths[..=position]
            .iter()
            .copied()
            .fold(px(0.), |sum, width| sum + width);
        let new_width = (pointer_x - table_left - scrolled - left_edge).max(MINIMUM_COLUMN_WIDTH);
        self.layout_dirty = true;
        self.column_widths.update(cx, |state, cx| {
            state.set_column_configuration(
                position + 1,
                new_width,
                TableResizeBehavior::MinSize(0.05),
            );
            cx.notify();
        });
    }

    /// Moves the column at one display position to another, taking its width with it.
    pub fn move_column(&mut self, from: usize, to: usize, window: &Window, cx: &mut Context<Self>) {
        if from == to || from >= self.column_order.len() || to >= self.column_order.len() {
            return;
        }
        let column = self.column_order.remove(from);
        self.column_order.insert(to, column);
        // Index 0 of the width state is the gutter.
        let mut widths = self.column_widths(window, cx);
        let width = widths.remove(from + 1);
        widths.insert(to + 1, width);
        self.column_widths.update(cx, |state, _| {
            for (index, width) in widths.into_iter().enumerate().skip(1) {
                state.set_column_configuration(index, width, TableResizeBehavior::MinSize(0.05));
            }
        });
        self.set_selection(None, cx);
        self.emit_layout(window, cx);
        cx.notify();
    }

    fn set_selection(&mut self, selection: Option<CellSelection>, cx: &mut Context<Self>) {
        self.set_selection_and_added_rows(selection, BTreeSet::new(), cx);
    }

    fn set_selection_and_added_rows(
        &mut self,
        selection: Option<CellSelection>,
        added_rows: BTreeSet<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.selection == selection && self.added_rows == added_rows {
            return;
        }
        self.selection = selection;
        self.added_rows = added_rows;
        self.publish_selection(cx);
    }

    /// Tells the inspector, and listeners, which rows are selected.
    fn publish_selection(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_source_rows();
        let active = ActiveSelection::shared(cx);
        let result = self.result.clone();
        let table_index = self.table_index;
        active.update(cx, |active, cx| {
            active.result = Some(result);
            active.table_index = table_index;
            active.rows = rows.clone();
            cx.notify();
        });
        cx.emit(ResultGridEvent::SelectionChanged { rows });
        cx.notify();
    }

    fn begin_selection(
        &mut self,
        row: usize,
        position: usize,
        modifiers: Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if modifiers.secondary() {
            self.select_row(row, modifiers, window, cx);
            return;
        }
        let only_this_cell = CellSelection::cell(row, position);
        if !modifiers.shift && self.selection == Some(only_this_cell) && self.added_rows.is_empty()
        {
            self.set_selection(None, cx);
            return;
        }
        let (selection, added_rows) = match self.selection {
            Some(existing) if modifiers.shift => {
                (existing.extended_to(row, position), self.added_rows.clone())
            }
            _ => (only_this_cell, BTreeSet::new()),
        };
        self.dragging_selection = true;
        self.dragging_rows = false;
        self.set_selection_and_added_rows(Some(selection), added_rows, cx);
        self.start_auto_scroll(window, cx);
    }

    fn extend_selection(&mut self, row: usize, position: usize, cx: &mut Context<Self>) {
        if !self.dragging_selection {
            return;
        }
        let Some(selection) = self.selection else {
            return;
        };
        let extended = if self.dragging_rows {
            CellSelection::rows(selection.anchor.0, row, self.column_order.len())
        } else {
            selection.extended_to(row, position)
        };
        let added_rows = self.added_rows.clone();
        self.set_selection_and_added_rows(Some(extended), added_rows, cx);
    }

    fn select_row(
        &mut self,
        row: usize,
        modifiers: Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let column_count = self.column_order.len();
        if modifiers.secondary() {
            let (selection, added_rows) =
                toggle_row(self.selection, &self.added_rows, row, column_count);
            self.set_selection_and_added_rows(selection, added_rows, cx);
            return;
        }
        let only_this_row = CellSelection::rows(row, row, column_count);
        if !modifiers.shift && self.selection == Some(only_this_row) && self.added_rows.is_empty() {
            self.set_selection(None, cx);
            return;
        }
        let (selection, added_rows) = match self.selection {
            Some(existing) if modifiers.shift => (
                CellSelection::rows(existing.anchor.0, row, column_count),
                self.added_rows.clone(),
            ),
            _ => (only_this_row, BTreeSet::new()),
        };
        self.dragging_selection = true;
        self.dragging_rows = true;
        self.set_selection_and_added_rows(Some(selection), added_rows, cx);
        self.start_auto_scroll(window, cx);
    }

    /// Shift+click on a header: selects the whole column, extends a whole-column selection to
    /// it, or clears the selection when it is the only selected column.
    fn select_column(&mut self, position: usize, cx: &mut Context<Self>) {
        let row_count = self.visible_rows.len();
        if row_count == 0 {
            return;
        }
        let only_this_column = CellSelection::columns(position, position, row_count);
        if self.selection == Some(only_this_column) {
            self.set_selection(None, cx);
            return;
        }
        let selection = match self.selection {
            Some(existing) if self.is_whole_columns(existing) => {
                CellSelection::columns(existing.anchor.1, position, row_count)
            }
            _ => only_this_column,
        };
        self.set_selection(Some(selection), cx);
    }

    /// Shift+click on the corner: selects the whole table, or clears it when already selected.
    fn select_everything(&mut self, cx: &mut Context<Self>) {
        let row_count = self.visible_rows.len();
        let everything = CellSelection::everything(row_count, self.column_order.len());
        if row_count == 0 || self.selection == Some(everything) {
            self.set_selection(None, cx);
        } else {
            self.set_selection(Some(everything), cx);
        }
    }

    fn is_whole_columns(&self, selection: CellSelection) -> bool {
        let last_row = self.visible_rows.len().saturating_sub(1);
        selection.anchor.0.min(selection.focus.0) == 0
            && selection.anchor.0.max(selection.focus.0) == last_row
    }

    fn is_whole_rows(&self, selection: CellSelection) -> bool {
        let last_column = self.column_order.len().saturating_sub(1);
        selection.anchor.1.min(selection.focus.1) == 0
            && selection.anchor.1.max(selection.focus.1) == last_column
    }

    /// Shows another part of the table, or all of it with `None`. The selection is cleared,
    /// as it is whenever the rows change.
    pub fn set_scope(&mut self, scope: Option<Arc<Vec<usize>>>, cx: &mut Context<Self>) {
        self.scope = scope;
        // The rows the selection referred to are going, and the inspector is told even when
        // this grid had nothing selected, because it may still show another grid's rows.
        self.selection = None;
        self.added_rows.clear();
        self.publish_selection(cx);
        self.recompute("Filtering", cx);
    }

    /// How many rows the grid starts from, before search and filters.
    pub fn scope_row_count(&self) -> usize {
        self.scope.as_ref().map_or_else(
            || {
                self.result
                    .tables
                    .get(self.table_index)
                    .map_or(0, |table| table.rows.len())
            },
            |scope| scope.len(),
        )
    }

    /// A click on the corner cell: ascending, then descending, then the original order.
    pub fn click_sort_row_number(&mut self, cx: &mut Context<Self>) {
        self.view_state.sort = self.view_state.sort.after_click(SortColumn::RowNumber);
        self.recompute("Sorting", cx);
    }

    pub fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.view_state.clear_filters();
        self.recompute("Filtering", cx);
    }

    fn deploy_context_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = self.focus_handle.clone();
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.context(focus_handle)
                .action("Copy", Box::new(Copy))
                .action("Copy as Markdown", Box::new(CopyAsMarkdown))
                .action("Copy as HTML", Box::new(CopyAsHtml))
                .action("Copy as datatable", Box::new(CopyAsDatatable))
        });
        window.focus(&menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu = None;
            cx.notify();
        });
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    /// While a drag is held above or below the rows, keeps extending the selection and scrolling.
    fn start_auto_scroll(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.auto_scroll_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTO_SCROLL_INTERVAL).await;
                let keep_going = this
                    .update_in(cx, |this, window, cx| this.auto_scroll_tick(window, cx))
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn auto_scroll_tick(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        if !self.dragging_selection {
            return false;
        }
        let bounds = self.table_bounds.get();
        let pointer = window.mouse_position();
        let rows_top = bounds.top() + HEADER_HEIGHT;
        // The pinned row numbers cover the left edge, so the columns begin after them.
        let columns_left = bounds.left()
            + self
                .column_widths(window, cx)
                .first()
                .copied()
                .unwrap_or_default();
        // The further the pointer is from an edge, the more rows or columns each tick covers.
        let step = |distance: Pixels| 1 + (f32::from(distance) / 40.) as isize;
        let row_delta = if pointer.y < rows_top {
            -step(rows_top - pointer.y)
        } else if pointer.y > bounds.bottom() {
            step(pointer.y - bounds.bottom())
        } else {
            0
        };
        let column_delta = if self.dragging_rows {
            0
        } else if pointer.x < columns_left {
            -step(columns_left - pointer.x)
        } else if pointer.x > bounds.right() {
            step(pointer.x - bounds.right())
        } else {
            0
        };
        if row_delta == 0 && column_delta == 0 {
            return true;
        }
        let Some(selection) = self.selection else {
            return true;
        };
        let last_row = self.visible_rows.len().saturating_sub(1) as isize;
        let last_column = self.column_order.len().saturating_sub(1) as isize;
        let row = (selection.focus.0 as isize + row_delta).clamp(0, last_row) as usize;
        let column = (selection.focus.1 as isize + column_delta).clamp(0, last_column) as usize;
        let extended = if self.dragging_rows {
            CellSelection::rows(selection.anchor.0, row, self.column_order.len())
        } else {
            selection.extended_to(row, column)
        };
        let added_rows = self.added_rows.clone();
        self.set_selection_and_added_rows(Some(extended), added_rows, cx);
        self.reveal_cell(row, column, window, cx);
        true
    }

    /// Scrolls so that the cell at this position of the visible rows and displayed columns
    /// shows.
    fn reveal_cell(&mut self, row: usize, column: usize, window: &Window, cx: &mut Context<Self>) {
        self.interaction_state
            .read(cx)
            .scroll_handle
            .scroll_to_item(row, ScrollStrategy::Nearest);
        self.reveal_column(column, window, cx);
        cx.notify();
    }

    fn reveal_column(&mut self, position: usize, window: &Window, cx: &mut Context<Self>) {
        let widths = self.column_widths(window, cx);
        let (Some(gutter), Some(width)) = (widths.first(), widths.get(position + 1)) else {
            return;
        };
        let viewport = self.table_bounds.get().size.width - *gutter;
        if viewport <= px(0.) {
            return;
        }
        let left: Pixels = widths[1..=position]
            .iter()
            .copied()
            .fold(px(0.), |sum, next| sum + next);
        let right = left + *width;
        let total: Pixels = widths[1..]
            .iter()
            .copied()
            .fold(px(0.), |sum, next| sum + next);
        let handle = self
            .interaction_state
            .read(cx)
            .horizontal_scroll_handle
            .clone();
        let offset = handle.offset();
        let wanted = if left + offset.x < px(0.) {
            -left
        } else if right + offset.x > viewport {
            viewport - right
        } else {
            offset.x
        };
        let scrolled = wanted.clamp((viewport - total).min(px(0.)), px(0.));
        if scrolled != offset.x {
            handle.set_offset(Point {
                x: scrolled,
                y: offset.y,
            });
        }
    }

    /// Moves the selected cell, or with `extend` the far corner of the selection.
    fn move_selection(
        &mut self,
        row_delta: isize,
        column_delta: isize,
        extend: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let row_count = self.visible_rows.len();
        let column_count = self.column_order.len();
        if row_count == 0 || column_count == 0 {
            return;
        }
        let Some(existing) = self.selection else {
            self.set_selection(Some(CellSelection::cell(0, 0)), cx);
            self.reveal_cell(0, 0, window, cx);
            return;
        };
        let row = (existing.focus.0 as isize + row_delta).clamp(0, row_count as isize - 1);
        let column = (existing.focus.1 as isize + column_delta).clamp(0, column_count as isize - 1);
        let (row, column) = (row as usize, column as usize);
        let selection = if extend {
            existing.extended_to(row, column)
        } else {
            CellSelection::cell(row, column)
        };
        self.set_selection(Some(selection), cx);
        self.reveal_cell(row, column, window, cx);
    }

    fn page_rows(&self) -> isize {
        self.last_rendered_rows.get().saturating_sub(1).max(1) as isize
    }

    fn end_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.dragging_selection = false;
        self.dragging_rows = false;
        self.auto_scroll_task = None;
        if std::mem::take(&mut self.layout_dirty) {
            self.emit_layout(window, cx);
        }
    }

    /// Shows the busy indicator if the work of this generation is still running after a
    /// quarter of a second.
    fn start_busy_indicator(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.busy_indicator_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BUSY_INDICATOR_DELAY).await;
            this.update(cx, |this, cx| {
                if this.busy && this.generation.load(Ordering::SeqCst) == generation {
                    this.show_busy_indicator = true;
                    cx.notify();
                }
            })
            .log_err();
        }));
    }

    fn recompute(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.busy = true;
        self.busy_action = action;
        self.show_busy_indicator = false;
        self.start_busy_indicator(generation, cx);
        cx.notify();

        let result = self.result.clone();
        let table_index = self.table_index;
        let view_state = self.view_state.clone();
        let scope = self.scope.clone();
        let latest = self.generation.clone();
        self.recompute_task = Some(cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn({
                    let latest = latest.clone();
                    async move {
                        let table = result.tables.get(table_index)?;
                        visible_rows(
                            table,
                            &view_state,
                            scope.as_deref().map(Vec::as_slice),
                            &|| latest.load(Ordering::SeqCst) == generation,
                        )
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if let Some(rows) = rows {
                    this.visible_rows = Arc::new(rows);
                }
                this.busy = false;
                this.show_busy_indicator = false;
                this.busy_indicator_task = None;
                // The positions a selection referred to no longer exist.
                this.set_selection(None, cx);
                cx.notify();
            })
            .log_err();
        }));
    }

    fn render_header(&self, position: usize, column: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(table) = self.result.tables.get(self.table_index) else {
            return div().into_any_element();
        };
        let name = table
            .columns
            .get(column)
            .map_or_else(SharedString::default, |column| {
                SharedString::from(column.name.clone())
            });
        let sorted = match self.view_state.sort.active {
            Some((SortColumn::Column(active), direction)) if active == column => Some(direction),
            _ => None,
        };
        let filtered = self.view_state.filters.contains_key(&column);
        let kind = table
            .columns
            .get(column)
            .map_or(kusto_results::ColumnKind::Other, |column| column.kind);
        let existing_filter = self.view_state.filters.get(&column).cloned();
        let popover_title = name.clone();
        let on_filter_change: FilterChanged = {
            let grid = cx.weak_entity();
            Rc::new(move |filter, cx| {
                grid.update(cx, |grid, cx| grid.set_filter(column, filter, cx))
                    .log_err();
            })
        };
        let dragged = DraggedHeader {
            position,
            label: name.clone(),
        };
        let column_selected = self.selection.is_some_and(|selection| {
            self.is_whole_columns(selection) && {
                let first = selection.anchor.1.min(selection.focus.1);
                let last = selection.anchor.1.max(selection.focus.1);
                (first..=last).contains(&position)
            }
        });
        let selected_color = cx.theme().colors().element_selected;
        h_flex()
            .id(("result-header", position))
            .when(column_selected, |header| header.bg(selected_color))
            .debug_selector(|| format!("header-cell-{position}"))
            .relative()
            .w_full()
            .gap_1()
            .items_center()
            .on_drop(
                cx.listener(move |this, dragged: &DraggedHeader, window, cx| {
                    this.move_column(dragged.position, position, window, cx)
                }),
            )
            .child(
                div()
                    .id(("result-header-name", position))
                    .debug_selector(|| format!("header-name-{position}"))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .child(name)
                    .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.modifiers().shift {
                            this.select_column(position, cx)
                        } else {
                            this.click_sort(column, cx)
                        }
                    })),
            )
            .child(
                div()
                    .debug_selector(|| format!("header-sort-{position}"))
                    .child(
                        IconButton::new(
                            ("result-header-sort", position),
                            match sorted {
                                Some(SortDirection::Descending) => IconName::ArrowDown,
                                _ => IconName::ArrowUp,
                            },
                        )
                        .icon_size(IconSize::XSmall)
                        .toggle_state(sorted.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| this.click_sort(column, cx))),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| format!("header-filter-{position}"))
                    .child(
                        PopoverMenu::new(("result-header-filter-menu", position))
                            .anchor(Anchor::TopRight)
                            .trigger(
                                IconButton::new(
                                    ("result-header-filter", position),
                                    IconName::FilterFunnel,
                                )
                                .icon_size(IconSize::XSmall)
                                .toggle_state(filtered),
                            )
                            .menu(move |window, cx| {
                                Some(cx.new(|cx| {
                                    FilterPopover::new(
                                        &popover_title,
                                        kind,
                                        existing_filter.as_ref(),
                                        on_filter_change.clone(),
                                        window,
                                        cx,
                                    )
                                }))
                            }),
                    ),
            )
            .child(
                div()
                    .id(("result-header-resize", position))
                    .debug_selector(|| format!("header-resize-{position}"))
                    .absolute()
                    .right(px(-6.))
                    .top_0()
                    .bottom_0()
                    .w(px(8.))
                    .cursor_col_resize()
                    .on_drag(
                        ResizeDrag {
                            position: Some(position),
                        },
                        |_, _, _, cx| cx.new(|_| Empty),
                    ),
            )
            .into_any_element()
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Vec<AnyElement>> {
        let Some(table) = self.result.tables.get(self.table_index) else {
            return Vec::new();
        };
        let selected_color = cx.theme().colors().element_selected;
        let settings = ResultsSettings::get_global(cx).clone();
        let severity_column = settings.trace_columns(table).severity;
        let rows: Vec<Vec<AnyElement>> = range
            .filter_map(|display_row| {
                let source_row = *self.visible_rows.get(display_row)?;
                let mut elements: Vec<AnyElement> = Vec::with_capacity(self.column_order.len() + 1);
                let row_selected = self.added_rows.contains(&display_row)
                    || self.selection.is_some_and(|selection| {
                        self.is_whole_rows(selection) && selection.contains(display_row, 0)
                    });
                let tint = severity_column
                    .and_then(|column| severity_level(table.cell(source_row, column)))
                    .and_then(|level| settings.severity_tint(level));
                elements.push(
                    div()
                        .size_full()
                        .debug_selector(|| format!("gutter-{display_row}"))
                        .when(row_selected, |cell| cell.bg(selected_color))
                        .child(SharedString::from((source_row + 1).to_string()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                window.focus(&this.focus_handle, cx);
                                this.select_row(display_row, event.modifiers, window, cx)
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                this.extend_selection(display_row, 0, cx)
                            }
                        }))
                        .into_any_element(),
                );
                for (position, &column) in self.column_order.iter().enumerate() {
                    let text = table
                        .cell(source_row, column)
                        .map(|cell| SharedString::from(cell.display_text().into_owned()))
                        .unwrap_or_default();
                    let selected = self.added_rows.contains(&display_row)
                        || self
                            .selection
                            .is_some_and(|selection| selection.contains(display_row, position));
                    elements.push(
                        div()
                            .size_full()
                            .debug_selector(|| format!("cell-{display_row}-{position}"))
                            .when_some(tint, |cell, tint| cell.bg(tint))
                            .when(selected, |cell| cell.bg(selected_color))
                            .child(text)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    window.focus(&this.focus_handle, cx);
                                    this.begin_selection(
                                        display_row,
                                        position,
                                        event.modifiers,
                                        window,
                                        cx,
                                    )
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                move |this, event: &MouseMoveEvent, _, cx| {
                                    if event.pressed_button == Some(MouseButton::Left) {
                                        this.extend_selection(display_row, position, cx)
                                    }
                                },
                            ))
                            .into_any_element(),
                    );
                }
                Some(elements)
            })
            .collect();
        self.last_rendered_rows.set(rows.len());
        rows
    }
}

impl Render for ResultGrid {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column_count = self.column_order.len() + 1;
        let mut headers: Vec<AnyElement> = Vec::with_capacity(column_count);
        let everything_selected = self.selection
            == Some(CellSelection::everything(
                self.visible_rows.len(),
                self.column_order.len(),
            ));
        let selected_color = cx.theme().colors().element_selected;
        headers.push(
            div()
                .id("result-corner")
                .debug_selector(|| "corner".to_string())
                .size_full()
                .relative()
                .cursor_pointer()
                .when(everything_selected, |corner| corner.bg(selected_color))
                .child("#")
                .child(
                    div()
                        .id("result-gutter-resize")
                        .debug_selector(|| "gutter-resize".to_string())
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(ResizeDrag { position: None }, |_, _, _, cx| {
                            cx.new(|_| Empty)
                        }),
                )
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    if event.modifiers().shift {
                        this.select_everything(cx)
                    } else {
                        this.click_sort_row_number(cx)
                    }
                }))
                .into_any_element(),
        );
        for (position, &column) in self.column_order.iter().enumerate() {
            headers.push(self.render_header(position, column, cx));
        }
        let row_count = self.visible_rows.len();
        let footer = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                Label::new(self.status_text())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when(self.shows_busy_indicator(), |footer| {
                let total = self.scope_row_count();
                let message = format!("{} {total} rows…", self.busy_action);
                footer.child(
                    h_flex()
                        .id("result-busy-indicator")
                        .debug_selector(|| "busy-indicator".to_string())
                        .role(gpui::Role::ProgressIndicator)
                        .aria_label(message.clone())
                        .gap_1()
                        .child(SpinnerLabel::new().size(LabelSize::Small))
                        .child(
                            Label::new(message)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
            });
        let no_rows_message = if self.scope_row_count() == 0 {
            "No results"
        } else {
            "No results match your search query"
        };
        let colors = cx.theme().colors();
        let has_filters = self
            .result
            .tables
            .get(self.table_index)
            .is_some_and(|table| self.view_state.has_active_filters(table));
        let search_open = self.search_open;
        let toolbar = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(colors.border)
            .child(
                div().debug_selector(|| "search-toggle".to_string()).child(
                    IconButton::new("result-search-toggle", IconName::MagnifyingGlass)
                        .icon_size(IconSize::Small)
                        .toggle_state(search_open)
                        .tooltip(Tooltip::text("Search"))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_search(window, cx)),
                        ),
                ),
            )
            .when(search_open, |toolbar| {
                toolbar.child(
                    div()
                        .debug_selector(|| "search".to_string())
                        .flex_1()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.editor_background)
                        .child(self.search_editor.clone()),
                )
            })
            .when(!search_open, |toolbar| toolbar.child(div().flex_1()))
            .when(has_filters, |toolbar| {
                toolbar.child(
                    div().debug_selector(|| "clear-filters".to_string()).child(
                        Button::new("result-clear-filters", "Clear all filters")
                            .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
                    ),
                )
            });
        v_flex()
            .size_full()
            .key_context("ResultGrid")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &CopyAsMarkdown, _, cx| this.copy_as_markdown(cx)))
            .on_action(cx.listener(|this, _: &CopyAsHtml, _, cx| this.copy_as_html(cx)))
            .on_action(cx.listener(|this, _: &CopyAsDatatable, _, cx| this.copy_as_datatable(cx)))
            .on_action(cx.listener(|this, _: &ToggleSearch, window, cx| this.toggle_search(window, cx)))
            .on_action(cx.listener(|this, _: &ClearAllFilters, _, cx| this.clear_filters(cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                let search_focused = this.search_editor.focus_handle(cx).is_focused(window);
                if search_focused && !this.search_editor.read(cx).text(cx).is_empty() {
                    this.search_editor.update(cx, |editor, cx| editor.clear(window, cx));
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &MoveUp, window, cx| this.move_selection(-1, 0, false, window, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, window, cx| this.move_selection(1, 0, false, window, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, window, cx| this.move_selection(0, -1, false, window, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, window, cx| this.move_selection(0, 1, false, window, cx)))
            .on_action(cx.listener(|this, _: &ExtendUp, window, cx| this.move_selection(-1, 0, true, window, cx)))
            .on_action(cx.listener(|this, _: &ExtendDown, window, cx| this.move_selection(1, 0, true, window, cx)))
            .on_action(cx.listener(|this, _: &ExtendLeft, window, cx| this.move_selection(0, -1, true, window, cx)))
            .on_action(cx.listener(|this, _: &ExtendRight, window, cx| this.move_selection(0, 1, true, window, cx)))
            .on_action(cx.listener(|this, _: &PageUp, window, cx| {
                let rows = this.page_rows();
                this.move_selection(-rows, 0, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, window, cx| {
                let rows = this.page_rows();
                this.move_selection(rows, 0, false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                let everything =
                    CellSelection::everything(this.visible_rows.len(), this.column_order.len());
                if !this.visible_rows.is_empty() {
                    this.set_selection(Some(everything), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                if this.selection.is_some() || !this.added_rows.is_empty() {
                    this.set_selection(None, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.deploy_context_menu(event.position, window, cx)
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.end_drag(window, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.end_drag(window, cx)),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeDrag>, window, cx| {
                    let position = event.drag(cx).position;
                    match position {
                        Some(position) => this.resize_column(
                            position,
                            event.event.position.x,
                            event.bounds.left(),
                            window,
                            cx,
                        ),
                        None => this.resize_gutter(
                            event.event.position.x - event.bounds.left(),
                            cx,
                        ),
                    }
                }),
            )
            .child(toolbar)
            .child(
                div().relative().flex_1().min_h_0().child(
                    Table::new(column_count)
                        .interactable(&self.interaction_state)
                        .width_config(ColumnWidthConfig::Resizable(self.column_widths.clone()))
                        .header(headers)
                        .empty_table_callback(move |_, _| {
                            div()
                                .p_3()
                                .child(Label::new(no_rows_message).color(Color::Muted))
                                .into_any_element()
                        })
                        .pin_cols(1)
                        .uniform_list(
                            "result-grid-rows",
                            row_count,
                            cx.processor(
                                |this: &mut ResultGrid,
                                 range: Range<usize>,
                                 _window: &mut Window,
                                 cx: &mut Context<ResultGrid>| {
                                    this.render_rows(range, cx)
                                },
                            ),
                        ),
                )
                .child(
                    canvas(
                        {
                            let table_bounds = self.table_bounds.clone();
                            move |bounds, _, _| table_bounds.set(bounds)
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                ),
            )
            .child(footer)
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use gpui::{Bounds, Modifiers, Point, ScrollStrategy, TestAppContext, VisualTestContext, size};
    use kusto_results::{Cell, Column, Table};

    use super::*;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    fn generated_result(row_count: usize, column_count: usize) -> ResultSet {
        let columns: Vec<Column> = (0..column_count)
            .map(|index| match index % 5 {
                0 => Column::new(format!("Id{index}"), "long"),
                1 => Column::new(format!("Message{index}"), "string"),
                2 => Column::new(format!("Stamp{index}"), "datetime"),
                3 => Column::new(format!("Payload{index}"), "dynamic"),
                _ => Column::new(format!("Note{index}"), "string"),
            })
            .collect();
        let rows = (0..row_count)
            .map(|row| {
                (0..column_count)
                    .map(|column| match column % 5 {
                        0 => Cell::Int((row * 7 + column) as i64),
                        1 => Cell::Text(format!(
                            "message {row} column {column} with some text to display"
                        )),
                        2 => Cell::Text(format!(
                            "2026-01-01T00:{:02}:{:02}.0000000Z",
                            row / 60 % 60,
                            row % 60
                        )),
                        3 if row % 3 == 0 => Cell::Null,
                        3 => Cell::Dynamic(Box::new(serde_json::json!({"row": row}))),
                        _ => Cell::Text(format!("note {}", row % 97)),
                    })
                    .collect()
            })
            .collect();
        ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns,
                rows,
            }],
            ..Default::default()
        }
    }

    fn draw(cx: &mut VisualTestContext) -> Duration {
        let started = Instant::now();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        started.elapsed()
    }

    fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
        sorted[((sorted.len() - 1) as f64 * fraction) as usize]
    }

    fn bounds_of(cx: &mut VisualTestContext, selector: &str) -> Bounds<Pixels> {
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("no element with selector {selector}"))
    }

    fn centre_of(cx: &mut VisualTestContext, selector: &str) -> Point<Pixels> {
        bounds_of(cx, selector).center()
    }

    fn click(cx: &mut VisualTestContext, selector: &str, modifiers: Modifiers) {
        let point = centre_of(cx, selector);
        cx.simulate_click(point, modifiers);
        draw(cx);
    }

    fn popover_is_open(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("filter-popover").is_some()
    }

    fn open_grid(
        cx: &mut TestAppContext,
        rows: usize,
        columns: usize,
    ) -> (Entity<ResultGrid>, &mut VisualTestContext) {
        init_test(cx);
        let result = Arc::new(generated_result(rows, columns));
        let (grid, cx) = cx.add_window_view(|window, cx| ResultGrid::new(result, 0, window, cx));
        cx.simulate_resize(size(px(1400.), px(700.)));
        draw(cx);
        (grid, cx)
    }

    fn sort_of(
        grid: &Entity<ResultGrid>,
        cx: &VisualTestContext,
    ) -> Option<(SortColumn, SortDirection)> {
        grid.read_with(cx, |grid, _| grid.view_state().sort.active)
    }

    #[gpui::test]
    async fn only_the_visible_rows_are_built(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 100_000, 12);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.visible_row_count()),
            100_000
        );
        let built = grid.read_with(cx, |grid, _| grid.last_rendered_rows());
        assert!(
            (1..200).contains(&built),
            "built {built} rows for a 700 px tall table over 100,000 rows"
        );
    }

    #[gpui::test]
    async fn sorting_runs_in_the_background_and_replaces_the_rows(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 1_000, 6);
        grid.update(cx, |grid, cx| grid.click_sort(0, cx));
        assert!(grid.read_with(cx, |grid, _| grid.is_busy()));
        cx.run_until_parked();
        assert!(!grid.read_with(cx, |grid, _| grid.is_busy()));
        grid.update(cx, |grid, cx| grid.click_sort(0, cx));
        cx.run_until_parked();
        let first = grid.read_with(cx, |grid, _| grid.visible_rows[0]);
        assert_eq!(first, 999);
    }

    /// S2: each header target fires on its own.
    #[gpui::test]
    async fn header_targets_are_independent(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);

        click(cx, "header-name-1", Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            sort_of(&grid, cx),
            Some((SortColumn::Column(1), SortDirection::Ascending)),
            "one click on the name sorts once"
        );

        click(cx, "header-filter-1", Modifiers::default());
        assert!(popover_is_open(cx), "the funnel opens the filter popover");
        assert_eq!(
            sort_of(&grid, cx),
            Some((SortColumn::Column(1), SortDirection::Ascending)),
            "the funnel does not sort"
        );

        click(cx, "header-name-1", Modifiers::default());
        cx.run_until_parked();
        click(cx, "header-name-1", Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            sort_of(&grid, cx),
            None,
            "the third click restores the original order"
        );

        click(cx, "header-sort-2", Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            sort_of(&grid, cx),
            Some((SortColumn::Column(2), SortDirection::Ascending)),
            "the sort button sorts"
        );
        assert!(
            !popover_is_open(cx),
            "a click on another target dismisses the popover"
        );
    }

    fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, by: Pixels) {
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        for step in 1..=6 {
            cx.simulate_mouse_move(
                Point {
                    x: from.x + by * (step as f32 / 6.),
                    y: from.y,
                },
                MouseButton::Left,
                Modifiers::default(),
            );
        }
        cx.simulate_mouse_up(
            Point {
                x: from.x + by,
                y: from.y,
            },
            MouseButton::Left,
            Modifiers::default(),
        );
        draw(cx);
    }

    /// S2: dragging the right edge of a header resizes the column and does not sort.
    #[gpui::test]
    async fn dragging_a_header_edge_resizes_without_sorting(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let before = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        let handle = centre_of(cx, "header-resize-1");
        drag(cx, handle, px(60.));
        let after = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        assert_eq!(before.len(), after.len());
        assert!(
            after[2] > before[2] + px(40.),
            "column width went from {:?} to {:?}",
            before[2],
            after[2]
        );
        assert_eq!(
            after[1], before[1],
            "the neighbour to the left is unchanged"
        );
        assert_eq!(
            after[3], before[3],
            "the neighbour to the right is unchanged"
        );
        assert_eq!(sort_of(&grid, cx), None, "a resize drag never sorts");

        // Dragging back past the minimum stops at the minimum width.
        let handle = centre_of(cx, "header-resize-1");
        drag(cx, handle, px(-600.));
        let shrunk = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        assert_eq!(shrunk[2], MINIMUM_COLUMN_WIDTH);
    }

    /// S2: the table's own dividers, which run down the body, also resize.
    #[gpui::test]
    async fn the_tables_body_dividers_resize_too(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let before = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        let divider = Point {
            x: before[..=2]
                .iter()
                .copied()
                .fold(px(0.), |sum, width| sum + width),
            y: px(100.),
        };
        drag(cx, divider, px(60.));
        let after = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        assert_eq!(after[2], before[2] + px(60.));
    }

    /// S2: a saved width follows its column when columns are reordered.
    #[gpui::test]
    async fn widths_follow_their_columns_when_reordered(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let handle = centre_of(cx, "header-resize-1");
        drag(cx, handle, px(60.));
        let widths = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        let wide = widths[2];
        assert!(wide > px(200.));
        let before_reorder = widths;

        let from = centre_of(cx, "header-name-1");
        let to = centre_of(cx, "header-name-4");
        drag(cx, from, to.x - from.x);
        let order = grid.read_with(cx, |grid, _| grid.column_order().to_vec());
        assert_eq!(order, [0, 2, 3, 4, 1, 5]);
        let widths = cx.update(|window, cx| grid.read(cx).column_widths(window, cx));
        assert_eq!(
            widths[5], wide,
            "the wide column moved to display position 4"
        );
        assert_eq!(
            widths[2], before_reorder[3],
            "the column that took its place brought its own width"
        );
    }

    /// S2: dragging a header onto another reorders the columns and their widths.
    #[gpui::test]
    async fn dragging_a_header_reorders_columns(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let from = centre_of(cx, "header-name-1");
        let to = centre_of(cx, "header-name-4");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        for step in 1..=10 {
            let x = from.x + (to.x - from.x) * (step as f32 / 10.);
            cx.simulate_mouse_move(
                Point { x, y: from.y },
                MouseButton::Left,
                Modifiers::default(),
            );
        }
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        draw(cx);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.column_order().to_vec()),
            [0, 2, 3, 4, 1, 5]
        );
        assert_eq!(sort_of(&grid, cx), None, "a drag is not a click");
    }

    /// S3: click, shift+click, drag and gutter clicks select rectangles of cells.
    #[gpui::test]
    async fn cells_select_as_rectangles(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let rows =
            |cx: &VisualTestContext| grid.read_with(cx, |grid, _| grid.selected_source_rows());

        click(cx, "cell-2-1", Modifiers::default());
        assert_eq!(rows(cx), [2]);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selection()),
            Some(CellSelection::cell(2, 1))
        );

        click(cx, "cell-5-3", Modifiers::shift());
        assert_eq!(rows(cx), [2, 3, 4, 5]);

        let from = centre_of(cx, "cell-1-0");
        let to = centre_of(cx, "cell-3-2");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        draw(cx);
        assert_eq!(rows(cx), [1, 2, 3]);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selection()),
            Some(CellSelection {
                anchor: (1, 0),
                focus: (3, 2)
            })
        );

        // Moving the mouse afterwards does not extend a finished drag.
        let outside = centre_of(cx, "cell-8-4");
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        assert_eq!(rows(cx), [1, 2, 3]);

        click(cx, "gutter-4", Modifiers::default());
        assert_eq!(rows(cx), [4]);
        click(cx, "gutter-6", Modifiers::shift());
        assert_eq!(rows(cx), [4, 5, 6]);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selection()),
            Some(CellSelection::rows(4, 6, 6))
        );
    }

    /// S3: a sort clears the selection, and the selection publishes source rows, not positions.
    #[gpui::test]
    async fn selection_follows_source_rows_and_clears_on_sort(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let _subscription = cx.update(|_, cx| {
            let events = events.clone();
            cx.subscribe(&grid, move |_, event: &ResultGridEvent, _| {
                if let Ok(mut events) = events.lock() {
                    events.push(event.clone());
                }
            })
        });

        click(cx, "header-name-0", Modifiers::default());
        click(cx, "header-name-0", Modifiers::default());
        cx.run_until_parked();
        // Id column ascending then descending: the last row is now first.
        click(cx, "cell-0-0", Modifiers::default());
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selected_source_rows()),
            [199]
        );

        click(cx, "header-name-1", Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selected_source_rows()),
            Vec::<usize>::new()
        );
        let events = events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default();
        assert_eq!(
            events,
            [
                ResultGridEvent::SelectionChanged { rows: vec![199] },
                ResultGridEvent::SelectionChanged { rows: Vec::new() },
            ]
        );
    }

    /// S5: the funnel opens an anchored popover whose input filters the rows as you type.
    #[gpui::test]
    async fn the_filter_popover_filters_as_you_type_and_closes(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        click(cx, "header-filter-1", Modifiers::default());
        assert!(popover_is_open(cx));

        let popover = bounds_of(cx, "filter-popover");
        assert!(
            popover.left() >= px(0.)
                && popover.right() <= px(1400.)
                && popover.bottom() <= px(700.),
            "the popover {popover:?} stays inside the window"
        );

        cx.simulate_input("message 17 ");
        cx.run_until_parked();
        draw(cx);
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 1);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().filters.len()),
            1
        );

        cx.dispatch_action(menu::Cancel);
        draw(cx);
        assert!(!popover_is_open(cx), "Escape closes the popover");
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.visible_row_count()),
            1,
            "closing keeps the filter"
        );

        click(cx, "header-filter-1", Modifiers::default());
        assert!(popover_is_open(cx), "it opens again");
        cx.simulate_click(
            Point {
                x: px(700.),
                y: px(500.),
            },
            Modifiers::default(),
        );
        draw(cx);
        assert!(!popover_is_open(cx), "a click outside closes it");

        click(cx, "header-filter-1", Modifiers::default());
        click(cx, "filter-clear", Modifiers::default());
        cx.run_until_parked();
        draw(cx);
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 200);
        assert!(!popover_is_open(cx), "Clear closes the popover");
    }

    /// FLT-3: a column takes two conditions, joined by all, and the second can be removed again.
    #[gpui::test]
    async fn a_column_can_have_two_conditions(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let visible =
            |cx: &VisualTestContext| grid.read_with(cx, |grid, _| grid.visible_row_count());
        click(cx, "header-filter-1", Modifiers::default());
        assert!(cx.debug_bounds("filter-remove").is_none());
        cx.simulate_input("message 1");
        cx.run_until_parked();
        draw(cx);
        assert_eq!(visible(cx), 111);

        click(cx, "filter-add", Modifiers::default());
        assert!(cx.debug_bounds("filter-value-1").is_some());
        assert!(cx.debug_bounds("filter-add").is_none(), "two is the most");
        cx.simulate_input("message 10");
        cx.run_until_parked();
        draw(cx);
        assert_eq!(visible(cx), 11, "both conditions must match");
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().filters[&1].conditions.len()),
            2
        );

        // The join list is drawn outside the popover, so a click on it is a click outside the
        // popover; that must not close the popover.
        click(cx, "filter-join", Modifiers::default());
        cx.simulate_click(
            Point {
                x: px(700.),
                y: px(500.),
            },
            Modifiers::default(),
        );
        draw(cx);
        assert!(
            popover_is_open(cx),
            "a click outside while a list is open keeps the popover"
        );

        click(cx, "filter-remove", Modifiers::default());
        cx.run_until_parked();
        draw(cx);
        assert_eq!(visible(cx), 111);
        assert!(cx.debug_bounds("filter-value-1").is_none());
    }

    /// S5: a popover anchored to its funnel's right edge would run off the left side of the
    /// window for the first column; the window keeps it on screen.
    #[gpui::test]
    async fn the_popover_stays_on_screen_for_the_first_column(cx: &mut TestAppContext) {
        let (_grid, cx) = open_grid(cx, 200, 6);
        let funnel = bounds_of(cx, "header-filter-0");
        click(cx, "header-filter-0", Modifiers::default());
        assert!(popover_is_open(cx));
        let popover = bounds_of(cx, "filter-popover");
        assert!(
            funnel.right() < popover.size.width,
            "this test needs a funnel closer to the left edge than the popover is wide"
        );
        assert!(
            popover.left() >= px(0.),
            "the popover {popover:?} is inside the window"
        );
        assert!(popover.right() <= px(1400.));
    }

    /// S5: Copy writes text to the clipboard: bare value for one cell, tab-separated otherwise.
    #[gpui::test]
    async fn copy_writes_the_selection_to_the_clipboard(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let clipboard = |cx: &mut VisualTestContext| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        };

        click(cx, "cell-3-0", Modifiers::default());
        grid.update(cx, |grid, cx| grid.copy_selection(cx));
        assert_eq!(clipboard(cx), "21", "one cell copies as its bare value");

        click(cx, "cell-4-1", Modifiers::shift());
        grid.update(cx, |grid, cx| grid.copy_selection(cx));
        let text = clipboard(cx);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Id0\tMessage1");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("21\tmessage 3 column 1"));

        grid.update(cx, |grid, cx| grid.set_selection(None, cx));
        grid.update(cx, |grid, cx| grid.copy_selection(cx));
        assert_eq!(
            clipboard(cx).lines().count(),
            201,
            "no selection copies the whole table"
        );
    }

    /// COL-4: a resize or a reorder reports the layout, and a grid opened with that layout shows
    /// the same order and widths.
    #[gpui::test]
    async fn the_column_layout_is_reported_and_restored(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 50, 4);
        let layouts = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&grid, {
                let layouts = layouts.clone();
                move |_, event: &ResultGridEvent, _| {
                    if let ResultGridEvent::LayoutChanged(layout) = event {
                        layouts.borrow_mut().push(layout.clone());
                    }
                }
            })
        });
        let widths = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| grid.read(cx).column_widths(window, cx))
        };

        let edge = centre_of(cx, "header-resize-1");
        drag(cx, edge, px(40.));
        assert_eq!(layouts.borrow().len(), 1, "one report, when the drag ends");
        let widened = widths(cx)[2];
        let from = centre_of(cx, "header-name-1");
        let to = centre_of(cx, "header-name-3");
        drag(cx, from, to.x - from.x);
        assert_eq!(layouts.borrow().len(), 2);

        let saved = layouts.borrow()[1].clone();
        assert_eq!(saved.name, "PrimaryResult");
        let saved_columns = saved.columns.clone().unwrap_or_default();
        let saved_order: Vec<usize> = saved_columns.iter().map(|column| column.index).collect();
        assert_eq!(saved_order, [0, 2, 3, 1]);
        assert_eq!(
            saved_columns[3].width,
            Some(f32::from(widened).round() as u32),
            "the widened column keeps its width in its new place"
        );

        let mut result = generated_result(50, 4);
        result.set_table_view(saved);
        let reopened =
            cx.update(|window, cx| cx.new(|cx| ResultGrid::new(Arc::new(result), 0, window, cx)));
        assert_eq!(
            reopened.read_with(cx, |grid, _| grid.column_order().to_vec()),
            [0, 2, 3, 1]
        );
        let whole = |widths: Vec<Pixels>| -> Vec<i32> {
            widths
                .into_iter()
                .map(|width| f32::from(width).round() as i32)
                .collect()
        };
        assert_eq!(
            whole(cx.update(|window, cx| reopened.read(cx).column_widths(window, cx))),
            whole(widths(cx)),
            "widths are saved in whole pixels"
        );
    }

    /// Keyboard: arrows move the selected cell, shift extends, and the view follows.
    #[gpui::test]
    async fn the_keyboard_moves_and_extends_the_selection(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let selection = |cx: &VisualTestContext| grid.read_with(cx, |grid, _| grid.selection());
        click(cx, "cell-2-1", Modifiers::default());

        cx.dispatch_action(MoveDown);
        cx.dispatch_action(MoveRight);
        assert_eq!(selection(cx), Some(CellSelection::cell(3, 2)));
        cx.dispatch_action(ExtendDown);
        cx.dispatch_action(ExtendDown);
        cx.dispatch_action(ExtendLeft);
        assert_eq!(
            selection(cx),
            Some(CellSelection {
                anchor: (3, 2),
                focus: (5, 1)
            })
        );
        cx.dispatch_action(MoveUp);
        assert_eq!(selection(cx), Some(CellSelection::cell(4, 1)));

        cx.dispatch_action(SelectAll);
        assert_eq!(selection(cx), Some(CellSelection::everything(200, 6)));
        cx.dispatch_action(ClearSelection);
        assert_eq!(selection(cx), None);
        cx.dispatch_action(MoveDown);
        assert_eq!(
            selection(cx),
            Some(CellSelection::cell(0, 0)),
            "it starts at the top"
        );

        for _ in 0..60 {
            cx.dispatch_action(MoveDown);
            draw(cx);
        }
        assert_eq!(selection(cx), Some(CellSelection::cell(60, 0)));
        let scrolled = grid.read_with(cx, |grid, cx| {
            grid.interaction_state().read(cx).scroll_offset()
        });
        assert!(
            scrolled.y < px(0.),
            "the view followed the selection: {scrolled:?}"
        );
        cx.dispatch_action(PageDown);
        let Some(paged) = selection(cx) else {
            panic!("a selection");
        };
        assert!(
            paged.focus.0 > 60 + 5,
            "a page moves several rows: {paged:?}"
        );
    }

    /// SEL-2: a drag held below the rows keeps extending the selection and scrolling.
    #[gpui::test]
    async fn dragging_past_the_bottom_scrolls_and_extends(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 500, 4);
        let from = centre_of(cx, "cell-2-1");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        let below = Point {
            x: from.x,
            y: px(690.),
        };
        cx.simulate_mouse_move(below, MouseButton::Left, Modifiers::default());
        for _ in 0..20 {
            cx.executor().advance_clock(AUTO_SCROLL_INTERVAL);
            cx.run_until_parked();
            draw(cx);
        }
        let Some(selection) = grid.read_with(cx, |grid, _| grid.selection()) else {
            panic!("a selection");
        };
        assert_eq!(selection.anchor, (2, 1));
        assert!(
            selection.focus.0 >= 20,
            "the drag reached {:?}",
            selection.focus
        );
        let scrolled = grid.read_with(cx, |grid, cx| {
            grid.interaction_state().read(cx).scroll_offset()
        });
        assert!(scrolled.y < px(0.), "the rows scrolled: {scrolled:?}");

        cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::default());
        let stopped = grid.read_with(cx, |grid, _| grid.selection());
        for _ in 0..5 {
            cx.executor().advance_clock(AUTO_SCROLL_INTERVAL);
            cx.run_until_parked();
        }
        assert_eq!(grid.read_with(cx, |grid, _| grid.selection()), stopped);
    }

    /// Moving with the keyboard to a column that is off screen scrolls sideways to it.
    #[gpui::test]
    async fn the_keyboard_scrolls_sideways_to_the_selected_column(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 50, 20);
        click(cx, "cell-0-0", Modifiers::default());
        for _ in 0..12 {
            cx.dispatch_action(MoveRight);
            draw(cx);
        }
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selection()),
            Some(CellSelection::cell(0, 12))
        );
        let offset = grid.read_with(cx, |grid, cx| {
            grid.interaction_state()
                .read(cx)
                .horizontal_scroll_handle
                .offset()
        });
        assert!(offset.x < px(0.), "the columns scrolled: {offset:?}");
        let bounds = bounds_of(cx, "cell-0-12");
        assert!(
            bounds.left() >= px(0.) && bounds.right() <= px(1400.) + px(1.),
            "the selected cell {bounds:?} is inside the window"
        );

        for _ in 0..12 {
            cx.dispatch_action(MoveLeft);
            draw(cx);
        }
        let back = grid.read_with(cx, |grid, cx| {
            grid.interaction_state()
                .read(cx)
                .horizontal_scroll_handle
                .offset()
        });
        assert_eq!(back.x, px(0.), "and back again");
    }

    /// SEL-2: a drag held beyond the right edge keeps extending the selection sideways.
    #[gpui::test]
    async fn dragging_past_the_right_edge_scrolls_sideways(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 50, 20);
        let from = centre_of(cx, "cell-2-1");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        let beyond = Point {
            x: px(1450.),
            y: from.y,
        };
        cx.simulate_mouse_move(beyond, MouseButton::Left, Modifiers::default());
        for _ in 0..20 {
            cx.executor().advance_clock(AUTO_SCROLL_INTERVAL);
            cx.run_until_parked();
            draw(cx);
        }
        let Some(selection) = grid.read_with(cx, |grid, _| grid.selection()) else {
            panic!("a selection");
        };
        assert!(
            selection.focus.1 >= 8,
            "the drag reached {:?}",
            selection.focus
        );
        let offset = grid.read_with(cx, |grid, cx| {
            grid.interaction_state()
                .read(cx)
                .horizontal_scroll_handle
                .offset()
        });
        assert!(offset.x < px(0.), "the columns scrolled: {offset:?}");
        cx.simulate_mouse_up(beyond, MouseButton::Left, Modifiers::default());
    }

    /// COL-1: the row-number gutter resizes, never below 40 px, and its width is saved.
    #[gpui::test]
    async fn the_gutter_resizes_and_reports_its_width(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 50, 4);
        let layouts = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&grid, {
                let layouts = layouts.clone();
                move |_, event: &ResultGridEvent, _| {
                    if let ResultGridEvent::LayoutChanged(layout) = event {
                        layouts.borrow_mut().push(layout.clone());
                    }
                }
            })
        });
        let gutter = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| grid.read(cx).column_widths(window, cx))[0]
        };
        let before = gutter(cx);
        let handle = centre_of(cx, "gutter-resize");
        drag(cx, handle, px(30.));
        assert!(
            gutter(cx) > before + px(20.),
            "{:?} to {:?}",
            before,
            gutter(cx)
        );
        assert_eq!(layouts.borrow().len(), 1);
        assert_eq!(
            layouts.borrow()[0].gutter_width,
            Some(f32::from(gutter(cx)).round() as u32)
        );
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().sort.active),
            None,
            "a resize is not a click"
        );

        let handle = centre_of(cx, "gutter-resize");
        drag(cx, handle, px(-500.));
        assert_eq!(gutter(cx), MINIMUM_GUTTER_WIDTH);
    }

    /// SRC-1, SRC-6, SRC-7, CMD-5, CMD-7: the search box is hidden until asked for, Escape clears
    /// it, hiding it clears the search, and the Clear All Filters action removes every filter.
    #[gpui::test]
    async fn the_search_box_toggles_and_a_hidden_search_does_not_filter(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let visible =
            |cx: &VisualTestContext| grid.read_with(cx, |grid, _| grid.visible_row_count());
        assert!(cx.debug_bounds("search").is_none(), "hidden by default");
        let grid_focus = grid.read_with(cx, |grid, _| grid.focus_handle.clone());
        cx.update(|window, cx| window.focus(&grid_focus, cx));

        cx.dispatch_action(ToggleSearch);
        draw(cx);
        assert!(grid.read_with(cx, |grid, _| grid.search_is_open()));
        assert!(cx.debug_bounds("search").is_some());
        cx.simulate_input("\"row\":13}");
        cx.run_until_parked();
        draw(cx);
        assert_eq!(
            visible(cx),
            1,
            "typing goes to the box, which has the focus"
        );

        cx.dispatch_action(Cancel);
        cx.run_until_parked();
        draw(cx);
        assert_eq!(visible(cx), 200, "Escape clears the search");
        assert!(
            grid.read_with(cx, |grid, _| grid.search_is_open()),
            "and keeps the box"
        );

        cx.simulate_input("\"row\":13}");
        cx.run_until_parked();
        assert_eq!(visible(cx), 1);
        cx.dispatch_action(ToggleSearch);
        cx.run_until_parked();
        draw(cx);
        assert!(cx.debug_bounds("search").is_none());
        assert_eq!(visible(cx), 200, "hiding the box clears its search");

        let filter = ColumnFilter {
            join: kusto_results::filter::Join::All,
            conditions: vec![kusto_results::filter::Condition::new(
                kusto_results::filter::FilterOperator::Contains,
                "message 7 column",
            )],
        };
        grid.update(cx, |grid, cx| grid.set_filter(1, Some(filter.clone()), cx));
        grid.update(cx, |grid, cx| grid.set_filter(4, Some(filter), cx));
        cx.run_until_parked();
        assert_eq!(visible(cx), 0);
        cx.update(|window, cx| window.focus(&grid_focus, cx));
        cx.dispatch_action(ClearAllFilters);
        cx.run_until_parked();
        assert_eq!(visible(cx), 200);
    }

    /// GRD-9: work still running after 250 ms shows a busy indicator with the row count, and it
    /// goes when the work is done. The work is held open by hand, because a test's background
    /// work finishes the moment anything runs.
    #[gpui::test]
    async fn long_work_shows_a_busy_indicator_after_a_quarter_of_a_second(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        grid.update(cx, |grid, cx| {
            grid.busy = true;
            grid.busy_action = "Sorting";
            let generation = grid.generation.load(Ordering::SeqCst);
            grid.start_busy_indicator(generation, cx);
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        draw(cx);
        assert!(!grid.read_with(cx, |grid, _| grid.shows_busy_indicator()));
        assert!(cx.debug_bounds("busy-indicator").is_none());

        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        draw(cx);
        assert!(grid.read_with(cx, |grid, _| grid.shows_busy_indicator()));
        assert!(cx.debug_bounds("busy-indicator").is_some());

        grid.update(cx, |grid, cx| grid.click_sort(2, cx));
        cx.run_until_parked();
        draw(cx);
        assert!(!grid.read_with(cx, |grid, _| grid.is_busy()));
        assert!(
            cx.debug_bounds("busy-indicator").is_none(),
            "it goes with the work"
        );

        grid.update(cx, |grid, cx| grid.click_sort(1, cx));
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(400));
        cx.run_until_parked();
        draw(cx);
        assert!(
            cx.debug_bounds("busy-indicator").is_none(),
            "quick work never shows it"
        );
    }

    /// ACT-6, ACT-13, ACT-14: a grid can show only some rows of its table, changing which
    /// clears the selection, and saves its layout under its own name.
    #[gpui::test]
    async fn a_scoped_grid_shows_only_its_rows_and_saves_its_own_layout(cx: &mut TestAppContext) {
        init_test(cx);
        let result = Arc::new(generated_result(50, 4));
        let (_root, cx) = cx.add_window_view(|_, _| Empty);
        let scope = Arc::new(vec![7, 3, 12]);
        let grid = cx.update(|window, cx| {
            cx.new(|cx| {
                ResultGrid::with_options(
                    result.clone(),
                    0,
                    GridOptions {
                        view_name: Some("PrimaryResult::activity-structured:0".into()),
                        scope: Some(scope),
                    },
                    window,
                    cx,
                )
            })
        });
        let rows = |cx: &mut VisualTestContext| {
            grid.read_with(cx, |grid, _| grid.visible_rows.as_ref().clone())
        };
        assert_eq!(rows(cx), [7, 3, 12], "in the order the scope gives");
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.status_text()),
            "3 rows",
            "the footer counts the scope, not the table"
        );

        grid.update(cx, |grid, cx| {
            grid.set_selection(Some(CellSelection::rows(1, 1, 4)), cx)
        });
        grid.update(cx, |grid, cx| {
            grid.set_scope(Some(Arc::new(vec![20, 21])), cx)
        });
        cx.run_until_parked();
        assert_eq!(rows(cx), [20, 21]);
        assert_eq!(grid.read_with(cx, |grid, _| grid.selection()), None);
        assert_eq!(
            cx.update(|_, cx| ActiveSelection::shared(cx).read(cx).rows.clone()),
            Vec::<usize>::new(),
            "an empty selection is published"
        );

        let layout = cx.update(|window, cx| grid.read(cx).layout(window, cx));
        assert_eq!(
            layout.map(|layout| layout.name),
            Some("PrimaryResult::activity-structured:0".to_string())
        );

        grid.update(cx, |grid, cx| grid.set_scope(None, cx));
        cx.run_until_parked();
        assert_eq!(rows(cx).len(), 50, "no scope is the whole table");
    }

    /// SEL-1, SEL-3 to SEL-5: clicking the only selected cell or row clears it, dragging on row
    /// numbers selects rows, and shift-clicking a header or the corner selects a column or the
    /// table.
    #[gpui::test]
    async fn gutter_header_and_corner_select_rows_columns_and_everything(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let selection = |cx: &VisualTestContext| grid.read_with(cx, |grid, _| grid.selection());

        click(cx, "cell-2-1", Modifiers::default());
        click(cx, "cell-2-1", Modifiers::default());
        assert_eq!(selection(cx), None, "the only selected cell clears");
        click(cx, "gutter-3", Modifiers::default());
        click(cx, "gutter-3", Modifiers::default());
        assert_eq!(selection(cx), None, "the only selected row clears");

        let from = centre_of(cx, "gutter-2");
        let to = centre_of(cx, "gutter-5");
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        draw(cx);
        assert_eq!(selection(cx), Some(CellSelection::rows(2, 5, 6)));

        click(cx, "header-name-1", Modifiers::shift());
        assert_eq!(selection(cx), Some(CellSelection::columns(1, 1, 200)));
        click(cx, "header-name-3", Modifiers::shift());
        assert_eq!(selection(cx), Some(CellSelection::columns(1, 3, 200)));
        click(cx, "header-name-1", Modifiers::shift());
        click(cx, "header-name-1", Modifiers::shift());
        assert_eq!(selection(cx), None, "the only selected column clears");
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().sort.active),
            None,
            "shift-click selects and does not sort"
        );

        click(cx, "corner", Modifiers::shift());
        assert_eq!(selection(cx), Some(CellSelection::everything(200, 6)));
        click(cx, "corner", Modifiers::shift());
        assert_eq!(selection(cx), None);

        click(cx, "corner", Modifiers::default());
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().sort.active),
            Some((SortColumn::RowNumber, SortDirection::Ascending))
        );
        click(cx, "corner", Modifiers::default());
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().sort.active),
            Some((SortColumn::RowNumber, SortDirection::Descending))
        );
        click(cx, "corner", Modifiers::default());
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.view_state().sort.active),
            None
        );
    }

    /// SRC-2, SRC-3, FLT-9: the search box narrows the rows, and Clear all filters appears only
    /// while a filter is active.
    #[gpui::test]
    async fn search_and_clear_all_filters(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 6);
        let search_editor = grid.read_with(cx, |grid, _| grid.search_editor.clone());
        search_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("\"row\":13}", window, cx)
        });
        cx.run_until_parked();
        draw(cx);
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 1);
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.status_text()),
            "1 of 200 rows"
        );
        assert!(cx.debug_bounds("clear-filters").is_none());

        search_editor.update_in(cx, |editor, window, cx| editor.set_text("", window, cx));
        cx.run_until_parked();
        let filter = ColumnFilter {
            join: kusto_results::filter::Join::All,
            conditions: vec![kusto_results::filter::Condition::new(
                kusto_results::filter::FilterOperator::Contains,
                "message 7 column",
            )],
        };
        grid.update(cx, |grid, cx| grid.set_filter(1, Some(filter), cx));
        cx.run_until_parked();
        draw(cx);
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 1);
        click(cx, "clear-filters", Modifiers::default());
        cx.run_until_parked();
        draw(cx);
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 200);
        assert!(cx.debug_bounds("clear-filters").is_none());
    }

    /// CPY-4, CPY-5, CPY-7: the context menu opens on a right click and the copy formats write
    /// their text.
    #[gpui::test]
    async fn the_context_menu_offers_the_copy_formats(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 20, 3);
        click(cx, "gutter-1", Modifiers::default());
        let over_rows = centre_of(cx, "cell-3-1");
        cx.simulate_mouse_down(over_rows, MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(over_rows, MouseButton::Right, Modifiers::default());
        draw(cx);
        assert!(grid.read_with(cx, |grid, _| grid.context_menu.is_some()));

        let clipboard = |cx: &mut VisualTestContext| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        };
        grid.update(cx, |grid, cx| grid.copy_as_markdown(cx));
        let markdown = clipboard(cx);
        assert!(
            markdown.starts_with("| Id0 | Message1 | Stamp2 |"),
            "{markdown}"
        );
        assert_eq!(markdown.lines().count(), 3, "header, separator and one row");
        grid.update(cx, |grid, cx| grid.copy_as_html(cx));
        assert!(clipboard(cx).contains("<table"));
        grid.update(cx, |grid, cx| grid.copy_as_datatable(cx));
        let expression = clipboard(cx);
        assert!(
            expression.starts_with("datatable (Id0: long, Message1: string, Stamp2: datetime) ["),
            "{expression}"
        );
        assert_eq!(
            expression.lines().count(),
            3,
            "header, one row and the bracket"
        );
    }

    /// SEL-11: Ctrl/Cmd+click on row numbers picks rows that are not next to each other, and
    /// the footer says how many rows are shown and selected.
    #[gpui::test]
    async fn rows_that_are_apart_can_be_selected_together(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 4);
        let published = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| ActiveSelection::shared(cx).read(cx).rows.clone())
        };
        let status = |cx: &mut VisualTestContext| grid.read_with(cx, |grid, _| grid.status_text());
        assert_eq!(status(cx), "200 rows");

        click(cx, "gutter-3", Modifiers::default());
        assert_eq!(status(cx), "200 rows · Row 4 selected");
        click(cx, "gutter-9", Modifiers::secondary_key());
        click(cx, "gutter-6", Modifiers::secondary_key());
        assert_eq!(published(cx), [3, 6, 9]);
        assert_eq!(status(cx), "200 rows · 3 rows selected");

        click(cx, "gutter-6", Modifiers::secondary_key());
        assert_eq!(published(cx), [3, 9]);

        grid.update(cx, |grid, cx| grid.copy_selection(cx));
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        assert_eq!(
            copied.lines().count(),
            3,
            "a header line and the two picked rows"
        );

        click(cx, "cell-12-1", Modifiers::secondary_key());
        assert_eq!(published(cx), [3, 9, 12], "a cell click picks its row too");

        click(cx, "gutter-2", Modifiers::default());
        assert_eq!(published(cx), [2], "a plain click starts over");
    }

    /// S6: a grid publishes its selection to the shared state the Row Details panel follows.
    #[gpui::test]
    async fn a_grid_publishes_its_selection_for_row_details(cx: &mut TestAppContext) {
        let (_grid, cx) = open_grid(cx, 200, 6);
        click(cx, "gutter-3", Modifiers::default());
        click(cx, "gutter-5", Modifiers::shift());
        let rows = cx.update(|_, cx| ActiveSelection::shared(cx).read(cx).rows.clone());
        assert_eq!(rows, [3, 4, 5]);
    }

    /// A sideways wheel moves the columns and leaves the rows where they are.
    #[gpui::test]
    async fn horizontal_wheel_scrolls_only_horizontally(cx: &mut TestAppContext) {
        let (grid, cx) = open_grid(cx, 200, 20);
        let state = grid.read_with(cx, |grid, _| grid.interaction_state().clone());
        let offsets = |cx: &mut VisualTestContext| {
            state.read_with(cx, |state, _| {
                (
                    state.scroll_offset(),
                    state.horizontal_scroll_handle.offset(),
                )
            })
        };
        let (vertical_before, _) = offsets(cx);
        let over_rows = centre_of(cx, "cell-3-2");
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: over_rows,
            delta: gpui::ScrollDelta::Lines(gpui::point(-3., 0.)),
            modifiers: Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
        let (vertical_after, horizontal_after) = offsets(cx);
        assert_ne!(horizontal_after.x, px(0.), "the columns scrolled");
        assert_eq!(vertical_after, vertical_before, "the rows did not scroll");

        cx.simulate_event(gpui::ScrollWheelEvent {
            position: over_rows,
            delta: gpui::ScrollDelta::Lines(gpui::point(0., -3.)),
            modifiers: Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
        let (vertical_scrolled, horizontal_unchanged) = offsets(cx);
        assert_ne!(
            vertical_scrolled.y,
            px(0.),
            "a vertical wheel scrolls the rows"
        );
        assert_eq!(
            horizontal_unchanged, horizontal_after,
            "and not the columns"
        );
    }

    /// RDT-11: closing the grid that owns the inspector's subject empties the inspector.
    #[gpui::test]
    async fn closing_the_grid_empties_the_inspector(cx: &mut TestAppContext) {
        init_test(cx);
        let result = Arc::new(generated_result(20, 3));
        let (_root, cx) = cx.add_window_view(|_, _| Empty);
        let grid = cx.update(|window, cx| cx.new(|cx| ResultGrid::new(result, 0, window, cx)));
        grid.update(cx, |grid, cx| {
            grid.set_selection(Some(CellSelection::rows(2, 2, 3)), cx)
        });
        let shown = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| ActiveSelection::shared(cx).read(cx).rows.clone())
        };
        assert_eq!(shown(cx), [2]);
        drop(grid);
        cx.update(|_, _| {});
        cx.run_until_parked();
        assert!(shown(cx).is_empty());
        let has_result = cx.update(|_, cx| ActiveSelection::shared(cx).read(cx).result.is_some());
        assert!(!has_result);
    }

    /// How long a real result takes to show: parsing it, building the grid and drawing the first
    /// frame. Set `KUSTO_TIMING_RESULT` to a `.ktt` file.
    #[gpui::test]
    #[ignore = "benchmark"]
    async fn open_time_of_a_real_result(cx: &mut TestAppContext) {
        init_test(cx);
        let path = std::env::var("KUSTO_TIMING_RESULT").expect("KUSTO_TIMING_RESULT is set");
        let text = std::fs::read_to_string(path).expect("the result file reads");

        let started = std::time::Instant::now();
        let rope = rope::Rope::from(text.as_str());
        println!(
            "Rope::from: {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        let started = std::time::Instant::now();
        let again = rope.to_string();
        println!(
            "Rope::to_string: {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        assert_eq!(again.len(), text.len());

        let started = std::time::Instant::now();
        let result = ResultSet::from_json(&text).expect("the file parses");
        println!("parse: {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
        println!(
            "{} rows x {} columns",
            result.tables[0].rows.len(),
            result.tables[0].columns.len()
        );

        let result = Arc::new(result);
        let started = std::time::Instant::now();
        let (_grid, cx) = cx.add_window_view(|window, cx| ResultGrid::new(result, 0, window, cx));
        println!(
            "grid built: {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        cx.simulate_resize(size(px(1600.), px(900.)));
        println!("first frame: {:.0} ms", draw(cx).as_secs_f64() * 1000.0);
        println!("second frame: {:.0} ms", draw(cx).as_secs_f64() * 1000.0);
    }

    /// Frame cost of building, laying out and painting the grid for 200,000 rows by 20 columns.
    /// `cargo test -p kusto_results_ui --profile release-fast -- --ignored --nocapture frame_time`
    #[gpui::test]
    #[ignore = "benchmark"]
    async fn frame_time_baseline(cx: &mut TestAppContext) {
        init_test(cx);
        let generated = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fork-docs/samples/generated/synthetic-large.ktt");
        let result = match std::fs::read_to_string(&generated) {
            Ok(text) => ResultSet::from_json(&text).expect("the generated file parses"),
            Err(_) => generated_result(200_000, 20),
        };
        let rows = result.tables[0].rows.len();
        let columns = result.tables[0].columns.len();
        let result = Arc::new(result);
        let (grid, cx) = cx.add_window_view(|window, cx| ResultGrid::new(result, 0, window, cx));
        cx.simulate_resize(size(px(1600.), px(900.)));

        let first = draw(cx);
        println!("{rows} rows x {columns} columns");
        println!("first frame: {:.1} ms", first.as_secs_f64() * 1000.0);
        println!(
            "rows built per frame: {}",
            grid.read_with(cx, |grid, _| grid.last_rendered_rows())
        );

        let mut timings = Vec::new();
        for step in 0..300usize {
            grid.update(cx, |grid, cx| {
                grid.interaction_state
                    .read(cx)
                    .scroll_handle
                    .scroll_to_item(step * 613, ScrollStrategy::Top);
            });
            timings.push(draw(cx));
        }
        timings.sort();
        println!(
            "scrolling, 300 frames: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            percentile(&timings, 0.5).as_secs_f64() * 1000.0,
            percentile(&timings, 0.95).as_secs_f64() * 1000.0,
            timings.last().copied().unwrap_or_default().as_secs_f64() * 1000.0,
        );

        let mut idle = Vec::new();
        for _ in 0..100 {
            cx.update(|window, _| window.refresh());
            idle.push(draw(cx));
        }
        idle.sort();
        println!(
            "redraw without scrolling, 100 frames: p50 {:.2} ms",
            percentile(&idle, 0.5).as_secs_f64() * 1000.0
        );

        for (label, column) in [("numeric", 0usize), ("datetime", 1), ("text", 3)] {
            let started = Instant::now();
            grid.update(cx, |grid, cx| grid.click_sort(column, cx));
            cx.run_until_parked();
            let sorted_in = started.elapsed();
            let frame = draw(cx);
            println!(
                "sort {label} column: {:.0} ms, first frame after {:.2} ms",
                sorted_in.as_secs_f64() * 1000.0,
                frame.as_secs_f64() * 1000.0
            );
            grid.update(cx, |grid, cx| grid.click_sort(column, cx));
            cx.run_until_parked();
            grid.update(cx, |grid, cx| grid.click_sort(column, cx));
            cx.run_until_parked();
        }

        let started = Instant::now();
        grid.update(cx, |grid, cx| grid.set_search("message 1999".into(), cx));
        cx.run_until_parked();
        println!(
            "search: {:.0} ms, {} rows left",
            started.elapsed().as_secs_f64() * 1000.0,
            grid.read_with(cx, |grid, _| grid.visible_row_count())
        );
    }
}
