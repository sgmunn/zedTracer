use std::cell::Cell;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    Anchor, AnyElement, ClipboardItem, Context, DefiniteLength, DragMoveEvent, Empty, Entity,
    EventEmitter, Length, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Render,
    SharedString, Task, Window, div, px,
};
use gpui_util::ResultExt as _;
use kusto_results::ResultSet;
use kusto_results::export::copy_text;
use kusto_results::filter::ColumnFilter;
use kusto_results::view::{
    CellSelection, SortColumn, SortDirection, ViewState, display_column_order, selected_positions,
    toggle_row, visible_rows,
};
use ui::{
    ColumnWidthConfig, IconButton, IconName, IconSize, PopoverMenu, ResizableColumnsState, Table,
    TableInteractionState, TableResizeBehavior, prelude::*,
};

use crate::filter_popover::{FilterChanged, FilterPopover};
use crate::row_details_panel::ActiveSelection;

/// `1234567` as `1,234,567`.
fn group_digits(number: usize) -> String {
    let digits = number.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

const GUTTER_WIDTH: Pixels = px(56.);
const DEFAULT_COLUMN_WIDTH: Pixels = px(160.);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultGridEvent {
    /// The selected rows changed, as source row indexes in display order.
    SelectionChanged { rows: Vec<usize> },
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
    position: usize,
}

const MINIMUM_COLUMN_WIDTH: Pixels = px(48.);

/// A results table: the rows of one table of a result set after search, filters and sort.
pub struct ResultGrid {
    result: Arc<ResultSet>,
    table_index: usize,
    view_state: ViewState,
    /// Original column indexes in the order they are displayed.
    column_order: Vec<usize>,
    visible_rows: Arc<Vec<usize>>,
    selection: Option<CellSelection>,
    /// Rows selected besides the rectangle, by position in the visible rows.
    added_rows: BTreeSet<usize>,
    dragging_selection: bool,
    interaction_state: Entity<TableInteractionState>,
    column_widths: Entity<ResizableColumnsState>,
    /// Bumped on every change to the view state; work for an older number is abandoned.
    generation: Arc<AtomicU64>,
    busy: bool,
    recompute_task: Option<Task<()>>,
    /// How many rows the last frame built, to show that only visible rows are created.
    last_rendered_rows: Cell<usize>,
}

impl EventEmitter<ResultGridEvent> for ResultGrid {}

impl ResultGrid {
    pub fn new(result: Arc<ResultSet>, table_index: usize, cx: &mut Context<Self>) -> Self {
        let table = result.tables.get(table_index);
        let table_rows = table.map_or(0, |table| table.rows.len());
        let column_count = table.map_or(0, |table| table.columns.len());
        let layout = table.and_then(|table| result.table_view(&table.name));
        let column_order = display_column_order(column_count, layout);

        let mut widths = vec![GUTTER_WIDTH];
        widths.extend(std::iter::repeat_n(DEFAULT_COLUMN_WIDTH, column_count));
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

        Self {
            result,
            table_index,
            view_state: ViewState::default(),
            column_order,
            visible_rows: Arc::new((0..table_rows).collect()),
            selection: None,
            added_rows: BTreeSet::new(),
            dragging_selection: false,
            interaction_state: cx.new(|cx| TableInteractionState::new(cx)),
            column_widths: cx
                .new(|_| ResizableColumnsState::new(column_count + 1, widths, behavior)),
            generation: Arc::new(AtomicU64::new(0)),
            busy: false,
            recompute_task: None,
            last_rendered_rows: Cell::new(0),
        }
    }

    pub fn last_rendered_rows(&self) -> usize {
        self.last_rendered_rows.get()
    }

    pub fn interaction_state(&self) -> &Entity<TableInteractionState> {
        &self.interaction_state
    }

    pub fn visible_row_count(&self) -> usize {
        self.visible_rows.len()
    }

    pub fn is_busy(&self) -> bool {
        self.busy
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
        let total = self
            .result
            .tables
            .get(self.table_index)
            .map_or(0, |table| table.rows.len());
        let shown = self.visible_rows.len();
        let mut parts = Vec::new();
        parts.push(if shown == total {
            format!("{} rows", group_digits(total))
        } else {
            format!("{} of {} rows", group_digits(shown), group_digits(total))
        });
        let selected = self.selected_source_rows();
        match selected.as_slice() {
            [] => {}
            [row] => parts.push(format!("Row {} selected", group_digits(row + 1))),
            rows => parts.push(format!("{} rows selected", group_digits(rows.len()))),
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
        self.recompute(cx);
    }

    pub fn set_search(&mut self, search: String, cx: &mut Context<Self>) {
        self.view_state.search = search;
        self.recompute(cx);
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
        self.recompute(cx);
    }

    /// Copies the selection, or the whole table when nothing is selected, as tab-separated
    /// text. One selected cell copies as its bare value.
    pub fn copy_selection(&self, cx: &mut Context<Self>) {
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
        cx.write_to_clipboard(ClipboardItem::new_string(copy_text(table, &rows, &columns)));
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
        cx: &mut Context<Self>,
    ) {
        if modifiers.secondary() {
            self.select_row(row, modifiers, cx);
            return;
        }
        let (selection, added_rows) = match self.selection {
            Some(existing) if modifiers.shift => {
                (existing.extended_to(row, position), self.added_rows.clone())
            }
            _ => (CellSelection::cell(row, position), BTreeSet::new()),
        };
        self.dragging_selection = true;
        self.set_selection_and_added_rows(Some(selection), added_rows, cx);
    }

    fn extend_selection(&mut self, row: usize, position: usize, cx: &mut Context<Self>) {
        if !self.dragging_selection {
            return;
        }
        if let Some(selection) = self.selection {
            let added_rows = self.added_rows.clone();
            self.set_selection_and_added_rows(
                Some(selection.extended_to(row, position)),
                added_rows,
                cx,
            );
        }
    }

    fn select_row(&mut self, row: usize, modifiers: Modifiers, cx: &mut Context<Self>) {
        let column_count = self.column_order.len();
        if modifiers.secondary() {
            let (selection, added_rows) =
                toggle_row(self.selection, &self.added_rows, row, column_count);
            self.set_selection_and_added_rows(selection, added_rows, cx);
            return;
        }
        let (selection, added_rows) = match self.selection {
            Some(existing) if modifiers.shift => (
                CellSelection::rows(existing.anchor.0, row, column_count),
                self.added_rows.clone(),
            ),
            _ => (CellSelection::rows(row, row, column_count), BTreeSet::new()),
        };
        self.set_selection_and_added_rows(Some(selection), added_rows, cx);
    }

    fn end_drag(&mut self) {
        self.dragging_selection = false;
    }

    fn recompute(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.busy = true;
        cx.notify();

        let result = self.result.clone();
        let table_index = self.table_index;
        let view_state = self.view_state.clone();
        let latest = self.generation.clone();
        self.recompute_task = Some(cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn({
                    let latest = latest.clone();
                    async move {
                        let table = result.tables.get(table_index)?;
                        visible_rows(table, &view_state, None, &|| {
                            latest.load(Ordering::SeqCst) == generation
                        })
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
        h_flex()
            .id(("result-header", position))
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
                    .on_click(cx.listener(move |this, _, _, cx| this.click_sort(column, cx))),
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
                    .on_drag(ResizeDrag { position }, |_, _, _, cx| cx.new(|_| Empty)),
            )
            .into_any_element()
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Vec<AnyElement>> {
        let Some(table) = self.result.tables.get(self.table_index) else {
            return Vec::new();
        };
        let selected_color = cx.theme().colors().element_selected;
        let rows: Vec<Vec<AnyElement>> = range
            .filter_map(|display_row| {
                let source_row = *self.visible_rows.get(display_row)?;
                let mut elements: Vec<AnyElement> = Vec::with_capacity(self.column_order.len() + 1);
                elements.push(
                    div()
                        .size_full()
                        .debug_selector(|| format!("gutter-{display_row}"))
                        .child(SharedString::from((source_row + 1).to_string()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.select_row(display_row, event.modifiers, cx)
                            }),
                        )
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
                            .when(selected, |cell| cell.bg(selected_color))
                            .child(text)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.begin_selection(display_row, position, event.modifiers, cx)
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
        headers.push(div().child("#").into_any_element());
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
            .when(self.busy, |footer| {
                footer.child(
                    Label::new("Working…")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            });
        v_flex()
            .size_full()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_drag()),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ResizeDrag>, window, cx| {
                    let position = event.drag(cx).position;
                    this.resize_column(
                        position,
                        event.event.position.x,
                        event.bounds.left(),
                        window,
                        cx,
                    );
                }),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Table::new(column_count)
                        .interactable(&self.interaction_state)
                        .width_config(ColumnWidthConfig::Resizable(self.column_widths.clone()))
                        .header(headers)
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
                ),
            )
            .child(footer)
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
        let (grid, cx) = cx.add_window_view(|_, cx| ResultGrid::new(result, 0, cx));
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
            x: px(376.),
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
        assert_eq!(widths[2], px(160.));
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

    #[test]
    fn numbers_are_grouped_in_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
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
        let grid = cx.new(|cx| ResultGrid::new(result, 0, cx));
        grid.update(cx, |grid, cx| {
            grid.set_selection(Some(CellSelection::rows(2, 2, 3)), cx)
        });
        let shown = |cx: &mut TestAppContext| {
            cx.update(|cx| ActiveSelection::shared(cx).read(cx).rows.clone())
        };
        assert_eq!(shown(cx), [2]);
        drop(grid);
        cx.update(|_| {});
        cx.run_until_parked();
        assert!(shown(cx).is_empty());
        let has_result = cx.update(|cx| ActiveSelection::shared(cx).read(cx).result.is_some());
        assert!(!has_result);
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
        let (grid, cx) = cx.add_window_view(|_, cx| ResultGrid::new(result, 0, cx));
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
