//! What a grid shows: the rows that survive search and filters, in sorted order, and the
//! small models around them (sort clicks, paging, selection, column order).
//!
//! The source table is never changed. A view is a list of source row indexes, so every
//! row keeps its identity through sorting and filtering.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Range;

use crate::filter::ColumnFilter;
use crate::result::{Cell, ColumnKind, Table, TableView};
use crate::typed::{NumberKey, cell_number, cell_ticks, natural_cmp};

/// How often long loops check whether their result is still wanted.
const CANCELLATION_CHECK_INTERVAL: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// The row-number gutter: ascending is the original result order.
    RowNumber,
    Column(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// At most one sorted column. `None` is the original result order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortState {
    pub active: Option<(SortColumn, SortDirection)>,
}

impl SortState {
    /// A click on a header: ascending, then descending, then back to the original order.
    /// Clicking another column starts that column at ascending.
    pub fn after_click(self, column: SortColumn) -> SortState {
        let active = match self.active {
            Some((current, SortDirection::Ascending)) if current == column => {
                Some((column, SortDirection::Descending))
            }
            Some((current, SortDirection::Descending)) if current == column => None,
            _ => Some((column, SortDirection::Ascending)),
        };
        SortState { active }
    }

    pub fn restore_result_order(self) -> SortState {
        SortState { active: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub search: String,
    pub filters: BTreeMap<usize, ColumnFilter>,
    pub sort: SortState,
}

impl ViewState {
    pub fn has_active_filters(&self, table: &Table) -> bool {
        self.filters.iter().any(|(column, filter)| {
            table
                .columns
                .get(*column)
                .is_some_and(|column| filter.usable(column.kind).is_some())
        })
    }

    pub fn clear_filters(&mut self) {
        self.filters.clear();
    }
}

/// The source rows to show, in display order, before paging.
///
/// `scope` limits the starting rows, as when a structured view shows one activity's events.
/// A row is kept when every whitespace-separated search term appears in some cell and every
/// column filter matches. Sorting then orders what is left; equal values stay in source
/// order in both directions. Returns `None` when `keep_going` turns false.
pub fn visible_rows(
    table: &Table,
    state: &ViewState,
    scope: Option<&[usize]>,
    keep_going: &dyn Fn() -> bool,
) -> Option<Vec<usize>> {
    let terms: Vec<String> = state
        .search
        .split_whitespace()
        .map(|term| term.to_lowercase())
        .collect();
    let filters: Vec<(usize, ColumnKind, ColumnFilter)> = state
        .filters
        .iter()
        .filter_map(|(column, filter)| {
            let kind = table.columns.get(*column)?.kind;
            Some((*column, kind, filter.usable(kind)?))
        })
        .collect();

    let candidates: Vec<usize> = match scope {
        Some(rows) => rows
            .iter()
            .copied()
            .filter(|row| *row < table.rows.len())
            .collect(),
        None => (0..table.rows.len()).collect(),
    };

    let mut kept = Vec::with_capacity(candidates.len());
    for (position, row_index) in candidates.into_iter().enumerate() {
        if position % CANCELLATION_CHECK_INTERVAL == 0 && !keep_going() {
            return None;
        }
        let row = &table.rows[row_index];
        let filters_match = filters.iter().all(|(column, kind, filter)| {
            row.get(*column)
                .is_some_and(|cell| filter.matches(cell, *kind))
        });
        if filters_match && row_matches_search(row, &terms) {
            kept.push(row_index);
        }
    }

    match state.sort.active {
        None | Some((SortColumn::RowNumber, SortDirection::Ascending)) => Some(kept),
        Some((SortColumn::RowNumber, SortDirection::Descending)) => {
            kept.reverse();
            Some(kept)
        }
        Some((SortColumn::Column(column), direction)) => {
            sort_by_column(table, kept, column, direction, keep_going)
        }
    }
}

fn row_matches_search(row: &[Cell], terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let texts: Vec<String> = row
        .iter()
        .map(|cell| cell.display_text().to_lowercase())
        .collect();
    terms
        .iter()
        .all(|term| texts.iter().any(|text| text.contains(term.as_str())))
}

/// A sortable value. Null is the smallest value; text that cannot be read as the column's
/// type sorts after everything else, in either direction.
enum SortKey {
    Null,
    Value(ValueKey),
    Unreadable,
}

enum ValueKey {
    Number(NumberKey),
    Ticks(i64),
    Bool(bool),
    Text(String),
}

fn sort_key(cell: &Cell, kind: ColumnKind) -> SortKey {
    if cell.is_null() {
        return SortKey::Null;
    }
    let key = match kind {
        ColumnKind::Int | ColumnKind::Long | ColumnKind::Real | ColumnKind::Decimal => {
            cell_number(cell, kind).map(ValueKey::Number)
        }
        ColumnKind::DateTime | ColumnKind::TimeSpan => cell_ticks(cell, kind).map(ValueKey::Ticks),
        ColumnKind::Bool => match cell {
            Cell::Bool(value) => Some(ValueKey::Bool(*value)),
            _ => None,
        },
        ColumnKind::String | ColumnKind::Guid | ColumnKind::Dynamic | ColumnKind::Other => {
            Some(ValueKey::Text(cell.display_text().to_lowercase()))
        }
    };
    key.map_or(SortKey::Unreadable, SortKey::Value)
}

fn compare_values(left: &ValueKey, right: &ValueKey) -> Ordering {
    match (left, right) {
        (ValueKey::Number(left), ValueKey::Number(right)) => left.compare(right),
        (ValueKey::Ticks(left), ValueKey::Ticks(right)) => left.cmp(right),
        (ValueKey::Bool(left), ValueKey::Bool(right)) => left.cmp(right),
        (ValueKey::Text(left), ValueKey::Text(right)) => natural_cmp(left, right),
        _ => Ordering::Equal,
    }
}

fn sort_by_column(
    table: &Table,
    rows: Vec<usize>,
    column: usize,
    direction: SortDirection,
    keep_going: &dyn Fn() -> bool,
) -> Option<Vec<usize>> {
    let kind = table.columns.get(column)?.kind;
    let mut keyed = Vec::with_capacity(rows.len());
    for (position, row_index) in rows.into_iter().enumerate() {
        if position % CANCELLATION_CHECK_INTERVAL == 0 && !keep_going() {
            return None;
        }
        let key = table
            .cell(row_index, column)
            .map_or(SortKey::Null, |cell| sort_key(cell, kind));
        keyed.push((row_index, key));
    }

    let descending = direction == SortDirection::Descending;
    keyed.sort_by(|(left_row, left_key), (right_row, right_key)| {
        let order = match (left_key, right_key) {
            (SortKey::Unreadable, SortKey::Unreadable) => Ordering::Equal,
            (SortKey::Unreadable, _) => return Ordering::Greater,
            (_, SortKey::Unreadable) => return Ordering::Less,
            (SortKey::Null, SortKey::Null) => Ordering::Equal,
            (SortKey::Null, SortKey::Value(_)) => Ordering::Less,
            (SortKey::Value(_), SortKey::Null) => Ordering::Greater,
            (SortKey::Value(left), SortKey::Value(right)) => {
                compare_values(left, right).then_with(|| {
                    // Case-insensitive text that is otherwise equal is ordered by the original.
                    match (
                        table.cell(*left_row, column),
                        table.cell(*right_row, column),
                    ) {
                        (Some(left), Some(right)) if kind_is_text(kind) => {
                            left.display_text().cmp(&right.display_text())
                        }
                        _ => Ordering::Equal,
                    }
                })
            }
        };
        let order = if descending { order.reverse() } else { order };
        order.then_with(|| left_row.cmp(right_row))
    });
    Some(keyed.into_iter().map(|(row, _)| row).collect())
}

fn kind_is_text(kind: ColumnKind) -> bool {
    matches!(
        kind,
        ColumnKind::String | ColumnKind::Guid | ColumnKind::Dynamic | ColumnKind::Other
    )
}

/// Severity 1 to 5: critical, error, warning, normal, verbose.
pub type SeverityLevel = u8;

/// The column holding severity: the first named `level` or `severity`, in any case.
pub fn severity_column(table: &Table) -> Option<usize> {
    table.columns.iter().position(|column| {
        let name = column.name.trim();
        name.eq_ignore_ascii_case("level") || name.eq_ignore_ascii_case("severity")
    })
}

/// The severity a cell holds, when it is a whole number from 1 to 5. Anything else, null
/// included, leaves the row untinted.
pub fn severity_level(cell: Option<&Cell>) -> Option<SeverityLevel> {
    let text = cell?.display_text();
    let value: f64 = text.trim().parse().ok()?;
    (value.fract() == 0.0 && (1.0..=5.0).contains(&value)).then_some(value as SeverityLevel)
}

/// The rows of one page, as positions in the visible list. The last page may be short, and a
/// page past the end is empty.
pub fn page_range(total_rows: usize, page_size: usize, page_index: usize) -> Range<usize> {
    let page_size = page_size.max(1);
    let start = page_index.saturating_mul(page_size).min(total_rows);
    start..start.saturating_add(page_size).min(total_rows)
}

/// The footer text, for example `Showing 1 to 1000 of 1240 rows`.
pub fn showing_label(page: &Range<usize>, total_rows: usize) -> String {
    let first = if page.is_empty() { 0 } else { page.start + 1 };
    format!("Showing {first} to {} of {total_rows} rows", page.end)
}

/// Page sizes the selector offers: the usual sizes plus the configured one.
pub fn page_size_options(configured: usize) -> Vec<usize> {
    let mut sizes = vec![50, 100, 500, 1000, 5000, configured.max(1)];
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// A rectangle of cells, in positions within the visible rows and the displayed columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSelection {
    pub anchor: (usize, usize),
    pub focus: (usize, usize),
}

impl CellSelection {
    pub fn cell(row: usize, column: usize) -> Self {
        Self {
            anchor: (row, column),
            focus: (row, column),
        }
    }

    /// Every column of the rows from `first_row` to `last_row`.
    pub fn rows(first_row: usize, last_row: usize, column_count: usize) -> Self {
        Self {
            anchor: (first_row, 0),
            focus: (last_row, column_count.saturating_sub(1)),
        }
    }

    /// Every row of the view for the columns from `first_column` to `last_column`.
    pub fn columns(first_column: usize, last_column: usize, row_count: usize) -> Self {
        Self {
            anchor: (0, first_column),
            focus: (row_count.saturating_sub(1), last_column),
        }
    }

    pub fn everything(row_count: usize, column_count: usize) -> Self {
        Self {
            anchor: (0, 0),
            focus: (row_count.saturating_sub(1), column_count.saturating_sub(1)),
        }
    }

    /// The source rows covered, in display order.
    pub fn source_rows(&self, visible_rows: &[usize]) -> Vec<usize> {
        let first = self.anchor.0.min(self.focus.0);
        let last = self.anchor.0.max(self.focus.0);
        visible_rows
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take(last - first + 1)
            .copied()
            .collect()
    }

    /// The original column indexes covered, in display order.
    pub fn source_columns(&self, column_order: &[usize]) -> Vec<usize> {
        let first = self.anchor.1.min(self.focus.1);
        let last = self.anchor.1.max(self.focus.1);
        column_order
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take(last - first + 1)
            .copied()
            .collect()
    }
}

/// The order columns are displayed in: the saved layout first, then any column it does not
/// mention in its original order. Out-of-range and repeated entries are ignored.
pub fn display_column_order(column_count: usize, layout: Option<&TableView>) -> Vec<usize> {
    let mut order = Vec::with_capacity(column_count);
    let mut seen = vec![false; column_count];
    for entry in layout
        .and_then(|view| view.columns.as_ref())
        .into_iter()
        .flatten()
    {
        if entry.index < column_count && !seen[entry.index] {
            seen[entry.index] = true;
            order.push(entry.index);
        }
    }
    order.extend((0..column_count).filter(|index| !seen[*index]));
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Condition, FilterOperator, Join};
    use crate::result::{Column, ColumnLayout};

    fn always() -> bool {
        true
    }

    #[test]
    fn severity_is_a_whole_number_from_one_to_five() {
        let table = table(&[("Message", "string"), (" Severity ", "long")], Vec::new());
        assert_eq!(severity_column(&table), Some(1));
        let level = |cell| severity_level(Some(&cell));
        assert_eq!(level(Cell::Int(3)), Some(3));
        assert_eq!(level(Cell::Real(2.0)), Some(2));
        assert_eq!(level(Cell::Text(" 5 ".into())), Some(5));
        for unreadable in [
            Cell::Int(0),
            Cell::Int(9),
            Cell::Real(2.5),
            Cell::Text("x".into()),
            Cell::Null,
        ] {
            assert_eq!(level(unreadable), None);
        }
        assert_eq!(severity_level(None), None);
    }

    fn table(columns: &[(&str, &str)], rows: Vec<Vec<Cell>>) -> Table {
        Table {
            name: "t".into(),
            columns: columns
                .iter()
                .map(|(name, type_name)| Column::new(*name, *type_name))
                .collect(),
            rows,
        }
    }

    fn text(value: &str) -> Cell {
        Cell::Text(value.into())
    }

    fn sorted(table: &Table, column: usize, direction: SortDirection) -> Vec<usize> {
        let state = ViewState {
            sort: SortState {
                active: Some((SortColumn::Column(column), direction)),
            },
            ..Default::default()
        };
        visible_rows(table, &state, None, &always).unwrap()
    }

    #[test]
    fn header_clicks_cycle_through_three_states() {
        let column = SortColumn::Column(2);
        let first = SortState::default().after_click(column);
        assert_eq!(first.active, Some((column, SortDirection::Ascending)));
        let second = first.after_click(column);
        assert_eq!(second.active, Some((column, SortDirection::Descending)));
        assert_eq!(second.after_click(column).active, None);
        let other = second.after_click(SortColumn::Column(3));
        assert_eq!(
            other.active,
            Some((SortColumn::Column(3), SortDirection::Ascending))
        );
        assert_eq!(other.restore_result_order().active, None);
    }

    #[test]
    fn numbers_sort_by_value_with_null_smallest() {
        let table = table(
            &[("n", "long")],
            vec![
                vec![Cell::Int(2)],
                vec![Cell::Int(10)],
                vec![Cell::Null],
                vec![Cell::Int(1)],
            ],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [2, 3, 0, 1]);
        assert_eq!(sorted(&table, 0, SortDirection::Descending), [1, 0, 3, 2]);
    }

    #[test]
    fn equal_values_stay_in_source_order_in_both_directions() {
        let table = table(
            &[("n", "long")],
            vec![
                vec![Cell::Int(1)],
                vec![Cell::Int(2)],
                vec![Cell::Int(1)],
                vec![Cell::Int(2)],
            ],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [0, 2, 1, 3]);
        assert_eq!(sorted(&table, 0, SortDirection::Descending), [1, 3, 0, 2]);
    }

    #[test]
    fn timespans_sort_by_duration() {
        let table = table(
            &[("t", "timespan")],
            vec![
                vec![text("10.00:00:00")],
                vec![text("1.00:00:00")],
                vec![text("-00:00:05")],
                vec![text("00:00:59")],
            ],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [2, 3, 1, 0]);
    }

    #[test]
    fn datetimes_sort_to_the_tick() {
        let table = table(
            &[("d", "datetime")],
            vec![
                vec![text("2025-01-01T00:00:00.0000007Z")],
                vec![text("2025-01-01T00:00:00.0000001Z")],
                vec![text("2024-12-31T23:59:59.9999999Z")],
            ],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [2, 1, 0]);
    }

    #[test]
    fn booleans_sort_false_first() {
        let table = table(
            &[("b", "bool")],
            vec![
                vec![Cell::Bool(true)],
                vec![Cell::Bool(false)],
                vec![Cell::Null],
            ],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [2, 1, 0]);
    }

    #[test]
    fn strings_sort_naturally_ignoring_case() {
        let table = table(
            &[("s", "string")],
            vec![
                vec![text("Step10")],
                vec![text("step2")],
                vec![text("Zeta")],
                vec![text("alpha")],
                vec![text("Step2")],
            ],
        );
        // "Step2" sorts before "step2" because case breaks the tie.
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [3, 4, 1, 0, 2]);
    }

    #[test]
    fn unreadable_values_sort_last_in_both_directions() {
        let table = table(
            &[("n", "long")],
            vec![vec![text("abc")], vec![Cell::Int(1)], vec![Cell::Null]],
        );
        assert_eq!(sorted(&table, 0, SortDirection::Ascending), [2, 1, 0]);
        assert_eq!(sorted(&table, 0, SortDirection::Descending), [1, 2, 0]);
    }

    #[test]
    fn row_number_sort_reverses_source_order() {
        let table = table(
            &[("n", "long")],
            vec![vec![Cell::Int(5)], vec![Cell::Int(1)]],
        );
        let mut state = ViewState::default();
        state.sort = state
            .sort
            .after_click(SortColumn::RowNumber)
            .after_click(SortColumn::RowNumber);
        assert_eq!(
            visible_rows(&table, &state, None, &always),
            Some(vec![1, 0])
        );
    }

    #[test]
    fn search_requires_every_term_somewhere_in_the_row() {
        let table = table(
            &[("a", "string"), ("b", "string")],
            vec![
                vec![text("Retry 1/3 scheduled"), text("westus2")],
                vec![text("retry later"), text("eastus")],
                vec![text("10:42:11"), text("café")],
            ],
        );
        let search = |query: &str| {
            let state = ViewState {
                search: query.into(),
                ..Default::default()
            };
            visible_rows(&table, &state, None, &always).unwrap()
        };
        assert_eq!(search("retry westus2"), [0]);
        assert_eq!(search("RETRY"), [0, 1]);
        assert_eq!(search("10:42"), [2]);
        assert_eq!(search("1/3"), [0]);
        assert_eq!(search("café"), [2]);
        assert_eq!(search("retry nothing"), Vec::<usize>::new());
        assert_eq!(search("   "), [0, 1, 2]);
    }

    #[test]
    fn search_filters_and_sort_combine_and_scope_limits_the_start() {
        let table = table(
            &[("level", "long"), ("message", "string")],
            vec![
                vec![Cell::Int(2), text("timeout")],
                vec![Cell::Int(4), text("timeout ok")],
                vec![Cell::Int(3), text("timeout retry")],
                vec![Cell::Int(1), text("unrelated")],
            ],
        );
        let mut state = ViewState {
            search: "timeout".into(),
            ..Default::default()
        };
        state.filters.insert(
            0,
            ColumnFilter {
                join: Join::All,
                conditions: vec![Condition::new(FilterOperator::LessThanOrEqual, "3")],
            },
        );
        state.sort = state.sort.after_click(SortColumn::Column(0));
        assert_eq!(
            visible_rows(&table, &state, None, &always),
            Some(vec![0, 2])
        );
        assert_eq!(
            visible_rows(&table, &state, Some(&[2, 3]), &always),
            Some(vec![2])
        );
        assert!(state.has_active_filters(&table));
        state.clear_filters();
        assert!(!state.has_active_filters(&table));
    }

    #[test]
    fn stops_when_the_result_is_no_longer_wanted() {
        let rows = (0..10_000).map(|value| vec![Cell::Int(value)]).collect();
        let table = table(&[("n", "long")], rows);
        assert_eq!(
            visible_rows(&table, &ViewState::default(), None, &|| false),
            None
        );
    }

    #[test]
    fn paging_reports_ranges_and_labels() {
        assert_eq!(page_range(1240, 1000, 0), 0..1000);
        assert_eq!(page_range(1240, 1000, 1), 1000..1240);
        assert_eq!(page_range(1240, 1000, 5), 1240..1240);
        assert_eq!(
            showing_label(&(1000..1240), 1240),
            "Showing 1001 to 1240 of 1240 rows"
        );
        assert_eq!(showing_label(&(0..0), 0), "Showing 0 to 0 of 0 rows");
        assert_eq!(page_size_options(250), [50, 100, 250, 500, 1000, 5000]);
        assert_eq!(page_size_options(1000), [50, 100, 500, 1000, 5000]);
    }

    #[test]
    fn selection_maps_visible_positions_to_source_rows_and_columns() {
        let visible = [7, 3, 9, 1];
        let order = [2, 0, 1];
        let selection = CellSelection::cell(1, 0);
        assert_eq!(selection.source_rows(&visible), [3]);
        assert_eq!(selection.source_columns(&order), [2]);
        let reversed = CellSelection {
            anchor: (2, 2),
            focus: (1, 1),
        };
        assert_eq!(reversed.source_rows(&visible), [3, 9]);
        assert_eq!(reversed.source_columns(&order), [0, 1]);
        assert_eq!(
            CellSelection::rows(0, 1, 3).source_columns(&order),
            [2, 0, 1]
        );
        assert_eq!(
            CellSelection::columns(0, 0, 4).source_rows(&visible),
            visible
        );
        assert_eq!(
            CellSelection::everything(4, 3).source_rows(&visible),
            visible
        );
        assert_eq!(
            CellSelection::cell(9, 0).source_rows(&visible),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn column_order_appends_missing_columns_and_ignores_bad_entries() {
        let layout = TableView {
            name: "t".into(),
            gutter_width: None,
            columns: Some(vec![
                ColumnLayout {
                    index: 2,
                    width: Some(100),
                },
                ColumnLayout {
                    index: 9,
                    width: None,
                },
                ColumnLayout {
                    index: 2,
                    width: None,
                },
                ColumnLayout {
                    index: 0,
                    width: None,
                },
            ]),
        };
        assert_eq!(display_column_order(4, Some(&layout)), [2, 0, 1, 3]);
        assert_eq!(display_column_order(3, None), [0, 1, 2]);
    }
}
