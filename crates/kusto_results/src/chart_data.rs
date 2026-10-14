//! Turns a table and the options of a chart into the series the chart draws.
//!
//! The rules follow the VS Code extension: what the options leave out is inferred from the
//! columns, rows are grouped into series by the series columns, and a row with no value to
//! draw is left out.

use std::collections::HashMap;
use std::fmt;

use chrono::DateTime;

use crate::chart::{ChartOptions, ChartType, SortOrder};
use crate::chart_scale;
use crate::result::{Cell, ColumnKind, Table};

/// What the X axis of a chart is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XKind {
    /// Milliseconds since the Unix epoch.
    Time,
    /// Milliseconds.
    Duration,
    Number,
    /// Positions among `ChartData::categories`.
    Category,
}

/// One value to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    /// The row of the table the point came from.
    pub row: usize,
}

/// A line, or one bar of every group, with a name for the legend.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    /// Its place among all the series before any were left out, which decides its colour.
    pub color_index: usize,
    /// Which of `ChartData::value_names` it reads.
    pub value_index: usize,
    /// In order of `x`.
    pub points: Vec<Point>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChartData {
    pub kind: ChartType,
    pub x_kind: XKind,
    pub x_name: String,
    /// The names of the columns that hold the values.
    pub value_names: Vec<String>,
    /// For a category axis: the label at each position.
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    /// How many series `max_series` left out.
    pub hidden_series: usize,
    /// Things to tell the user about how the chart was read, such as a column that was skipped.
    pub notes: Vec<String>,
}

/// Why a chart cannot be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartProblem {
    /// The chart's type is one this fork does not draw.
    Unsupported(String),
    NoColumns,
    MissingColumn(String),
    NoValueColumn,
    NoPoints,
}

impl fmt::Display for ChartProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChartProblem::Unsupported(chart_type) if chart_type.is_empty() => {
                formatter.write_str("This chart has no type.")
            }
            ChartProblem::Unsupported(chart_type) => {
                write!(formatter, "A {chart_type} chart cannot be drawn here.")
            }
            ChartProblem::NoColumns => formatter.write_str("The table has no columns."),
            ChartProblem::MissingColumn(name) => {
                write!(formatter, "The table has no column {name}.")
            }
            ChartProblem::NoValueColumn => {
                formatter.write_str("There is no numeric column to draw.")
            }
            ChartProblem::NoPoints => formatter.write_str("There is nothing to draw."),
        }
    }
}

impl std::error::Error for ChartProblem {}

/// The type to start a chart with when the user adds one to a table with no `render`.
pub fn suggest_chart_type(table: &Table) -> ChartType {
    let has =
        |wanted: fn(ColumnKind) -> bool| table.columns.iter().any(|column| wanted(column.kind));
    let has_numbers = has(ColumnKind::is_numeric);
    if has_numbers && has(|kind| kind == ColumnKind::DateTime) {
        ChartType::TimeLine
    } else if has_numbers && has(|kind| kind == ColumnKind::String) {
        ChartType::Column
    } else if has_numbers {
        ChartType::Line
    } else {
        ChartType::Column
    }
}

impl ChartData {
    pub fn build(table: &Table, options: &ChartOptions) -> Result<Self, ChartProblem> {
        let kind = options
            .kind()
            .filter(|kind| kind.is_drawn())
            .ok_or_else(|| ChartProblem::Unsupported(options.chart_type.clone()))?;
        if table.columns.is_empty() {
            return Err(ChartProblem::NoColumns);
        }
        let mut notes = Vec::new();
        let columns = ChartColumns::resolve(table, options, kind, &mut notes)?;

        let x_column = &table.columns[columns.x];
        let x_kind = match (x_column.kind, kind.is_bars()) {
            (ColumnKind::DateTime, false) => XKind::Time,
            (ColumnKind::TimeSpan, false) => XKind::Duration,
            (kind, false) if kind.is_numeric() => XKind::Number,
            _ => XKind::Category,
        };

        let value_count = columns.values.len();
        let mut categories = CategoryList::new(x_column.kind);
        let mut group_ids: HashMap<String, usize> = HashMap::new();
        let mut group_names: Vec<String> = Vec::new();
        // Points of each group, with one list for each value column.
        let mut group_points: Vec<Vec<Vec<Point>>> = Vec::new();
        for (row_index, row) in table.rows.iter().enumerate() {
            let Some(x_cell) = row.get(columns.x) else {
                continue;
            };
            let x = match x_kind {
                XKind::Category => match categories.position(x_cell) {
                    Some(position) => position as f64,
                    None => continue,
                },
                XKind::Time => match parse_datetime(x_cell) {
                    Some(x) => x,
                    None => continue,
                },
                XKind::Duration => match parse_timespan(x_cell) {
                    Some(x) => x,
                    None => continue,
                },
                XKind::Number => match numeric(x_cell) {
                    Some(x) => x,
                    None => continue,
                },
            };
            let group = if columns.series.is_empty() {
                0
            } else {
                let key = series_key(row, &columns.series);
                match group_ids.get(&key) {
                    Some(group) => *group,
                    None => {
                        group_ids.insert(key.clone(), group_names.len());
                        group_names.push(key);
                        group_names.len() - 1
                    }
                }
            };
            while group_points.len() <= group {
                group_points.push(vec![Vec::new(); value_count]);
            }
            for (value_index, column) in columns.values.iter().enumerate() {
                let Some(y) = row.get(*column).and_then(numeric) else {
                    continue;
                };
                group_points[group][value_index].push(Point {
                    x,
                    y,
                    row: row_index,
                });
            }
        }

        let mut categories_in_order = categories.into_labels_and_order();
        let mut series = Vec::new();
        for (group, per_value) in group_points.into_iter().enumerate() {
            for (value_index, points) in per_value.into_iter().enumerate() {
                if points.is_empty() {
                    continue;
                }
                let value_name = &table.columns[columns.values[value_index]].name;
                let name = match (columns.series.is_empty(), value_count) {
                    (true, _) => value_name.clone(),
                    (false, 1) => group_names[group].clone(),
                    (false, _) => format!("{} - {value_name}", group_names[group]),
                };
                series.push(Series {
                    name,
                    color_index: group * value_count + value_index,
                    value_index,
                    points,
                });
            }
        }
        if series.is_empty() {
            return Err(ChartProblem::NoPoints);
        }

        if x_kind == XKind::Category {
            let order = category_order(&series, &categories_in_order, options.sort);
            let mut new_position = vec![0usize; categories_in_order.labels.len()];
            for (position, old) in order.iter().enumerate() {
                new_position[*old] = position;
            }
            for series in &mut series {
                for point in &mut series.points {
                    point.x = new_position[point.x as usize] as f64;
                }
            }
            categories_in_order.labels = order
                .iter()
                .map(|old| std::mem::take(&mut categories_in_order.labels[*old]))
                .collect();
        }
        for series in &mut series {
            series.points.sort_by(|a, b| a.x.total_cmp(&b.x));
            if options.accumulate == Some(true) {
                let mut total = 0.0;
                for point in &mut series.points {
                    total += point.y;
                    point.y = total;
                }
            }
        }

        let total_series = series.len();
        let mut hidden_series = 0;
        if let Some(limit) = options.max_series.filter(|limit| *limit > 0)
            && series.len() > limit as usize
        {
            let magnitude =
                |series: &Series| -> f64 { series.points.iter().map(|point| point.y.abs()).sum() };
            let mut ranked: Vec<(usize, f64)> = series.iter().map(magnitude).enumerate().collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut keep: Vec<usize> = ranked
                .into_iter()
                .take(limit as usize)
                .map(|(index, _)| index)
                .collect();
            keep.sort_unstable();
            series = keep
                .into_iter()
                .map(|index| series[index].clone())
                .collect();
            hidden_series = total_series - series.len();
            notes.push(format!(
                "Showing the {} largest of {total_series} series.",
                series.len()
            ));
        }

        Ok(ChartData {
            kind,
            x_kind,
            x_name: x_column.name.clone(),
            value_names: columns
                .values
                .iter()
                .map(|column| table.columns[*column].name.clone())
                .collect(),
            categories: categories_in_order.labels,
            series,
            hidden_series,
            notes,
        })
    }

    /// The smallest and largest X of all points, or `None` with no points.
    pub fn x_extent(&self) -> Option<(f64, f64)> {
        extent(
            self.series
                .iter()
                .flat_map(|series| series.points.iter().map(|point| point.x)),
        )
    }

    /// The smallest and largest Y of all points, or `None` with no points.
    pub fn y_extent(&self) -> Option<(f64, f64)> {
        extent(
            self.series
                .iter()
                .flat_map(|series| series.points.iter().map(|point| point.y)),
        )
    }
}

fn extent(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    values.fold(None, |extent, value| match extent {
        None => Some((value, value)),
        Some((low, high)) => Some((low.min(value), high.max(value))),
    })
}

/// The columns a chart reads, as positions in the table.
struct ChartColumns {
    x: usize,
    values: Vec<usize>,
    series: Vec<usize>,
}

impl ChartColumns {
    fn resolve(
        table: &Table,
        options: &ChartOptions,
        kind: ChartType,
        notes: &mut Vec<String>,
    ) -> Result<Self, ChartProblem> {
        let find = |name: &String| table.column_index(name);
        let anomaly: Vec<usize> = options
            .anomaly_columns
            .iter()
            .flatten()
            .filter_map(find)
            .collect();
        let given_values: Vec<usize> = options
            .y_columns
            .iter()
            .flatten()
            .filter_map(|name| {
                let index = find(name);
                if index.is_none() {
                    notes.push(format!("The table has no column {name}."));
                }
                index
            })
            .collect();
        let given_series: Vec<usize> = options
            .series_columns
            .iter()
            .flatten()
            .filter_map(|name| {
                let index = find(name);
                if index.is_none() {
                    notes.push(format!("The table has no column {name}."));
                }
                index
            })
            .collect();

        let x = match options.x_column.as_ref().filter(|name| !name.is_empty()) {
            Some(name) => find(name).ok_or_else(|| ChartProblem::MissingColumn(name.clone()))?,
            None => infer_x(table, kind, &given_values, &anomaly),
        };

        let numeric_column = |index: &usize| table.columns[*index].kind.is_numeric();
        let values: Vec<usize> = if options
            .y_columns
            .as_ref()
            .is_some_and(|names| !names.is_empty())
        {
            given_values
                .into_iter()
                .filter(|index| {
                    let draws = numeric_column(index);
                    if !draws {
                        notes.push(format!(
                            "The column {} is not numeric, so it is not drawn.",
                            table.columns[*index].name
                        ));
                    }
                    draws
                })
                .collect()
        } else {
            (0..table.columns.len())
                .filter(|index| {
                    *index != x
                        && !given_series.contains(index)
                        && !anomaly.contains(index)
                        && numeric_column(index)
                })
                .collect()
        };
        if values.is_empty() {
            return Err(ChartProblem::NoValueColumn);
        }

        let series = if given_series.is_empty() {
            (0..table.columns.len())
                .find(|index| {
                    let column_kind = table.columns[*index].kind;
                    *index != x
                        && !values.contains(index)
                        && !anomaly.contains(index)
                        && !column_kind.is_numeric()
                        && !matches!(column_kind, ColumnKind::DateTime | ColumnKind::TimeSpan)
                })
                .into_iter()
                .collect()
        } else {
            given_series
        };
        Ok(Self { x, values, series })
    }
}

/// The X column when the options name none: for a time chart the first datetime column that is
/// not a value, otherwise the first column that is not a value, otherwise the first column.
fn infer_x(table: &Table, kind: ChartType, values: &[usize], anomaly: &[usize]) -> usize {
    let free = |index: usize| !values.contains(&index) && !anomaly.contains(&index);
    if kind.prefers_time_axis()
        && let Some(index) = (0..table.columns.len())
            .find(|index| table.columns[*index].kind == ColumnKind::DateTime && free(*index))
    {
        return index;
    }
    if !values.is_empty()
        && let Some(index) = (0..table.columns.len()).find(|index| free(*index))
    {
        return index;
    }
    (0..table.columns.len())
        .find(|index| !anomaly.contains(index))
        .unwrap_or(0)
}

fn series_key(row: &[Cell], series_columns: &[usize]) -> String {
    let mut key = String::new();
    for (position, column) in series_columns.iter().enumerate() {
        if position > 0 {
            key.push_str(", ");
        }
        match row.get(*column).map(Cell::display_text) {
            Some(text) if !text.is_empty() => key.push_str(&text),
            _ => key.push_str("(empty)"),
        }
    }
    key
}

/// The labels of a category axis, in the order they were first met, and what each is sorted by.
struct CategoryList {
    column_kind: ColumnKind,
    positions: HashMap<String, usize>,
    labels: Vec<String>,
    /// For categories that are really numbers or times: the value to put them in order by.
    sort_keys: Vec<f64>,
}

struct CategoryOrder {
    labels: Vec<String>,
    /// The sort key of each label, or all `NaN` for labels that stay in the order they came.
    sort_keys: Vec<f64>,
}

impl CategoryList {
    fn new(column_kind: ColumnKind) -> Self {
        Self {
            column_kind,
            positions: HashMap::new(),
            labels: Vec::new(),
            sort_keys: Vec::new(),
        }
    }

    /// The position of the cell's category, adding it when it is new. A null has none.
    fn position(&mut self, cell: &Cell) -> Option<usize> {
        if cell.is_null() {
            return None;
        }
        let key = cell.display_text().into_owned();
        if let Some(position) = self.positions.get(&key) {
            return Some(*position);
        }
        let sort_key = match self.column_kind {
            ColumnKind::DateTime => parse_datetime(cell),
            ColumnKind::TimeSpan => parse_timespan(cell),
            kind if kind.is_numeric() => numeric(cell),
            _ => None,
        };
        let position = self.labels.len();
        self.positions.insert(key.clone(), position);
        self.labels.push(key);
        self.sort_keys.push(sort_key.unwrap_or(f64::NAN));
        Some(position)
    }

    fn into_labels_and_order(self) -> CategoryOrder {
        let mut labels = self.labels;
        let ordered_by_value =
            !self.sort_keys.is_empty() && self.sort_keys.iter().all(|key| !key.is_nan());
        if ordered_by_value {
            match self.column_kind {
                ColumnKind::DateTime => {
                    let with_time = self
                        .sort_keys
                        .iter()
                        .any(|key| key.rem_euclid(86_400_000.0) != 0.0);
                    labels = self
                        .sort_keys
                        .iter()
                        .map(|key| chart_scale::format_time_label(*key, with_time))
                        .collect();
                }
                ColumnKind::TimeSpan => {
                    labels = self
                        .sort_keys
                        .iter()
                        .map(|key| chart_scale::format_duration(*key))
                        .collect();
                }
                _ => {}
            }
        }
        CategoryOrder {
            labels,
            sort_keys: if ordered_by_value {
                self.sort_keys
            } else {
                vec![f64::NAN; self.sort_keys.len()]
            },
        }
    }
}

/// The old positions of the categories in the order they are drawn: by total when asked,
/// else by value for numbers and times, else as they came.
fn category_order(
    series: &[Series],
    categories: &CategoryOrder,
    sort: Option<SortOrder>,
) -> Vec<usize> {
    let count = categories.labels.len();
    let mut order: Vec<usize> = (0..count).collect();
    match sort {
        Some(SortOrder::Ascending | SortOrder::Descending) => {
            let mut totals = vec![0.0f64; count];
            for series in series {
                for point in &series.points {
                    if let Some(total) = totals.get_mut(point.x as usize) {
                        *total += point.y;
                    }
                }
            }
            order.sort_by(|a, b| totals[*a].total_cmp(&totals[*b]));
            if sort == Some(SortOrder::Descending) {
                order.reverse();
            }
        }
        _ => {
            if categories.sort_keys.iter().all(|key| !key.is_nan()) && count > 0 {
                order.sort_by(|a, b| categories.sort_keys[*a].total_cmp(&categories.sort_keys[*b]));
            }
        }
    }
    order
}

/// A number a chart can draw. Kusto writes NaN and infinities as text, and they have no place.
fn numeric(cell: &Cell) -> Option<f64> {
    let value = match cell {
        Cell::Int(value) => *value as f64,
        Cell::Real(value) => *value,
        Cell::Decimal(text) | Cell::Text(text) => text.trim().parse().ok()?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn parse_datetime(cell: &Cell) -> Option<f64> {
    let Cell::Text(text) = cell else {
        return None;
    };
    parse_datetime_text(text)
}

/// Milliseconds since the epoch of an ISO 8601 time with a zone, as Kusto writes datetimes.
pub fn parse_datetime_text(text: &str) -> Option<f64> {
    let date = DateTime::parse_from_rfc3339(text).ok()?;
    Some(date.timestamp() as f64 * 1000.0 + f64::from(date.timestamp_subsec_nanos()) / 1e6)
}

fn parse_timespan(cell: &Cell) -> Option<f64> {
    let Cell::Text(text) = cell else {
        return None;
    };
    parse_timespan_text(text)
}

/// Milliseconds in a timespan written `[-][d.]hh:mm:ss[.fffffff]`.
pub fn parse_timespan_text(text: &str) -> Option<f64> {
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut parts = rest.split(':');
    let (head, minutes, seconds) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (days, hours) = match head.split_once('.') {
        Some((days, hours)) => (days.parse::<f64>().ok()?, hours.parse::<f64>().ok()?),
        None => (0.0, head.parse::<f64>().ok()?),
    };
    let total = ((days * 24.0 + hours) * 60.0 + minutes.parse::<f64>().ok()?) * 60.0
        + seconds.parse::<f64>().ok()?;
    let milliseconds = total * 1000.0;
    Some(if negative {
        -milliseconds
    } else {
        milliseconds
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::chart::Chart;

    fn table(columns: &[(&str, &str)], rows: Vec<Vec<Cell>>) -> Table {
        Table {
            name: "t".into(),
            columns: columns
                .iter()
                .map(|(name, type_name)| crate::Column::new(*name, *type_name))
                .collect(),
            rows,
        }
    }

    fn text(value: &str) -> Cell {
        Cell::Text(value.into())
    }

    fn options(value: serde_json::Value) -> ChartOptions {
        Chart::from_value(json!({"options": value}))
            .map(|chart| chart.options)
            .unwrap_or_default()
    }

    fn day(number: u32) -> Cell {
        text(&format!("2007-01-{number:02}T00:00:00Z"))
    }

    fn names(data: &ChartData) -> Vec<&str> {
        data.series
            .iter()
            .map(|series| series.name.as_str())
            .collect()
    }

    fn ys(series: &Series) -> Vec<f64> {
        series.points.iter().map(|point| point.y).collect()
    }

    fn events() -> Table {
        table(
            &[
                ("StartTime", "datetime"),
                ("EventType", "string"),
                ("Events", "long"),
            ],
            vec![
                vec![day(2), text("Hail"), Cell::Int(5)],
                vec![day(1), text("Flood"), Cell::Int(35)],
                vec![day(1), text("Hail"), Cell::Int(4)],
                vec![day(2), text("Flood"), Cell::Int(7)],
            ],
        )
    }

    #[test]
    fn a_time_chart_groups_rows_into_series_and_sorts_each_by_time() {
        let data = ChartData::build(&events(), &options(json!({"type": "TimeLine"}))).unwrap();
        assert_eq!(data.x_kind, XKind::Time);
        assert_eq!(data.x_name, "StartTime");
        assert_eq!(
            names(&data),
            ["Hail", "Flood"],
            "in the order they were first met"
        );
        assert_eq!(ys(&data.series[0]), [4.0, 5.0]);
        assert_eq!(ys(&data.series[1]), [35.0, 7.0]);
        assert_eq!(data.series[0].points[0].row, 2, "points know their row");
        assert_eq!(data.series[1].color_index, 1);
        assert_eq!(data.value_names, ["Events"]);
        assert_eq!(
            data.x_extent().map(|(low, _)| low),
            parse_datetime_text("2007-01-01T00:00:00Z")
        );
        assert_eq!(data.y_extent(), Some((4.0, 35.0)));
    }

    #[test]
    fn a_time_chart_prefers_a_datetime_column_over_the_first_column() {
        let table = table(
            &[("Name", "string"), ("When", "datetime"), ("Count", "long")],
            vec![
                vec![text("a"), day(1), Cell::Int(1)],
                vec![text("a"), day(2), Cell::Int(2)],
            ],
        );
        let data = ChartData::build(&table, &options(json!({"type": "TimeLine"}))).unwrap();
        assert_eq!(data.x_name, "When");
        assert_eq!(names(&data), ["a"], "the string column splits the series");
        let data = ChartData::build(&table, &options(json!({"type": "Line"}))).unwrap();
        assert_eq!(data.x_name, "Name", "a line chart takes the first column");
        assert_eq!(data.x_kind, XKind::Category);
    }

    #[test]
    fn the_options_choose_the_columns_when_they_name_them() {
        let options = options(json!({
            "type": "Line", "xColumn": "StartTime", "yColumns": ["Events"],
            "seriesColumns": ["EventType"]
        }));
        let data = ChartData::build(&events(), &options).unwrap();
        assert_eq!(names(&data), ["Hail", "Flood"]);

        let data = ChartData::build(
            &events(),
            &options_with(&options, |options| {
                options.series_columns = Some(Vec::new())
            }),
        )
        .unwrap();
        assert_eq!(
            names(&data),
            ["Hail", "Flood"],
            "an empty list is the same as none"
        );

        let missing = options_with(&options, |options| options.x_column = Some("When".into()));
        assert_eq!(
            ChartData::build(&events(), &missing),
            Err(ChartProblem::MissingColumn("When".into()))
        );
    }

    fn options_with(base: &ChartOptions, change: impl FnOnce(&mut ChartOptions)) -> ChartOptions {
        let mut options = base.clone();
        change(&mut options);
        options
    }

    #[test]
    fn several_value_columns_become_several_series() {
        let table = table(
            &[("Region", "string"), ("Total", "long"), ("Average", "real")],
            vec![
                vec![text("east"), Cell::Int(10), Cell::Real(2.5)],
                vec![text("west"), Cell::Int(20), Cell::Real(4.5)],
            ],
        );
        let data = ChartData::build(&table, &options(json!({"type": "Column"}))).unwrap();
        assert_eq!(names(&data), ["Total", "Average"]);
        assert_eq!(data.categories, ["east", "west"]);
        assert_eq!(data.series[1].value_index, 1);

        let table = table_with_series_and_two_values();
        let data = ChartData::build(&table, &options(json!({"type": "Column"}))).unwrap();
        assert_eq!(names(&data), ["x - a", "x - b", "y - a", "y - b"]);
        assert_eq!(
            data.series
                .iter()
                .map(|series| series.color_index)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    fn table_with_series_and_two_values() -> Table {
        table(
            &[
                ("Key", "string"),
                ("Kind", "string"),
                ("a", "long"),
                ("b", "long"),
            ],
            vec![
                vec![text("k"), text("x"), Cell::Int(1), Cell::Int(2)],
                vec![text("k"), text("y"), Cell::Int(3), Cell::Int(4)],
            ],
        )
    }

    #[test]
    fn rows_without_a_value_to_draw_are_left_out() {
        let table = table(
            &[("T", "datetime"), ("A", "long"), ("B", "real")],
            vec![
                vec![day(1), Cell::Int(1), Cell::Real(1.5)],
                vec![Cell::Null, Cell::Int(2), Cell::Real(2.5)],
                vec![day(3), Cell::Null, Cell::Text("NaN".into())],
                vec![day(4), Cell::Int(4), Cell::Decimal("4.25".into())],
            ],
        );
        let data = ChartData::build(&table, &options(json!({"type": "Line"}))).unwrap();
        assert_eq!(ys(&data.series[0]), [1.0, 4.0]);
        assert_eq!(ys(&data.series[1]), [1.5, 4.25]);
    }

    #[test]
    fn a_column_that_is_not_numeric_is_skipped_with_a_note() {
        let options = options(
            json!({"type": "Line", "xColumn": "StartTime", "yColumns": ["Events", "EventType", "Nope"]}),
        );
        let data = ChartData::build(&events(), &options).unwrap();
        assert_eq!(data.value_names, ["Events"]);
        assert_eq!(data.notes.len(), 2);
        assert!(data.notes.iter().any(|note| note.contains("EventType")));
        assert!(data.notes.iter().any(|note| note.contains("Nope")));

        let only_text = options_with(&options, |options| {
            options.y_columns = Some(vec!["EventType".into()])
        });
        assert_eq!(
            ChartData::build(&events(), &only_text),
            Err(ChartProblem::NoValueColumn)
        );
    }

    #[test]
    fn accumulate_adds_up_each_series_in_order_of_x() {
        let data = ChartData::build(
            &events(),
            &options(json!({"type": "Line", "accumulate": true})),
        )
        .unwrap();
        assert_eq!(ys(&data.series[0]), [4.0, 9.0]);
        assert_eq!(ys(&data.series[1]), [35.0, 42.0]);
    }

    #[test]
    fn max_series_keeps_the_largest_and_says_so() {
        let rows = (1..=5)
            .flat_map(|series| {
                (1..=2).map(move |step| {
                    vec![
                        day(step),
                        text(&format!("s{series}")),
                        Cell::Int(i64::from(series * 10 + step as i32)),
                    ]
                })
            })
            .collect();
        let table = table(&[("T", "datetime"), ("S", "string"), ("V", "long")], rows);
        let data =
            ChartData::build(&table, &options(json!({"type": "Line", "maxSeries": 2}))).unwrap();
        assert_eq!(names(&data), ["s4", "s5"]);
        assert_eq!(data.hidden_series, 3);
        assert_eq!(
            data.series[0].color_index, 3,
            "a series keeps its colour when others are left out"
        );
        assert!(data.notes[0].contains("2 largest of 5"));
    }

    #[test]
    fn categories_keep_their_order_unless_sorted_by_total() {
        let table = table(
            &[("State", "string"), ("N", "long")],
            vec![
                vec![text("TX"), Cell::Int(5)],
                vec![text("CA"), Cell::Int(9)],
                vec![text("NY"), Cell::Int(1)],
            ],
        );
        let build = |sort: &str| {
            ChartData::build(&table, &options(json!({"type": "Bar", "sort": sort}))).unwrap()
        };
        assert_eq!(build("Auto").categories, ["TX", "CA", "NY"]);
        assert_eq!(build("Descending").categories, ["CA", "TX", "NY"]);
        assert_eq!(build("Ascending").categories, ["NY", "TX", "CA"]);
        let sorted = build("Descending");
        assert_eq!(
            sorted.series[0]
                .points
                .iter()
                .map(|point| point.x)
                .collect::<Vec<_>>(),
            [0.0, 1.0, 2.0]
        );
        assert_eq!(ys(&sorted.series[0]), [9.0, 5.0, 1.0]);
    }

    #[test]
    fn bars_over_times_are_categories_in_time_order() {
        let table = table(
            &[("T", "datetime"), ("N", "long")],
            vec![
                vec![day(3), Cell::Int(3)],
                vec![day(1), Cell::Int(1)],
                vec![day(2), Cell::Int(2)],
            ],
        );
        let data = ChartData::build(&table, &options(json!({"type": "Column"}))).unwrap();
        assert_eq!(data.x_kind, XKind::Category);
        assert_eq!(data.categories, ["2007-01-01", "2007-01-02", "2007-01-03"]);
        assert_eq!(ys(&data.series[0]), [1.0, 2.0, 3.0]);

        let with_time = table_with_times();
        let data = ChartData::build(&with_time, &options(json!({"type": "Column"}))).unwrap();
        assert_eq!(data.categories, ["2007-01-01 00:00", "2007-01-01 06:30"]);
    }

    fn table_with_times() -> Table {
        table(
            &[("T", "datetime"), ("N", "long")],
            vec![
                vec![text("2007-01-01T06:30:00Z"), Cell::Int(2)],
                vec![text("2007-01-01T00:00:00Z"), Cell::Int(1)],
            ],
        )
    }

    #[test]
    fn numbers_on_a_line_chart_are_an_axis_and_on_a_bar_chart_are_labels() {
        let table = table(
            &[("Size", "long"), ("Count", "long")],
            vec![
                vec![Cell::Int(10), Cell::Int(1)],
                vec![Cell::Int(2), Cell::Int(5)],
            ],
        );
        let line = ChartData::build(&table, &options(json!({"type": "Line"}))).unwrap();
        assert_eq!(line.x_kind, XKind::Number);
        assert_eq!(line.series[0].points[0].x, 2.0, "sorted by x");
        let bars = ChartData::build(&table, &options(json!({"type": "Column"}))).unwrap();
        assert_eq!(bars.categories, ["2", "10"], "numbers are in numeric order");
    }

    #[test]
    fn a_chart_that_cannot_be_drawn_says_why() {
        let build = |value| ChartData::build(&events(), &options(value));
        assert_eq!(
            build(json!({"type": "Sankey"})),
            Err(ChartProblem::Unsupported("Sankey".into()))
        );
        assert_eq!(
            build(json!({})),
            Err(ChartProblem::Unsupported(String::new()))
        );
        let empty = table(&[], Vec::new());
        assert_eq!(
            ChartData::build(&empty, &options(json!({"type": "Line"}))),
            Err(ChartProblem::NoColumns)
        );
        let no_rows = table(&[("T", "datetime"), ("N", "long")], Vec::new());
        assert_eq!(
            ChartData::build(&no_rows, &options(json!({"type": "Line"}))),
            Err(ChartProblem::NoPoints)
        );
        let no_numbers = table(
            &[("T", "datetime"), ("S", "string")],
            vec![vec![day(1), text("a")]],
        );
        assert_eq!(
            ChartData::build(&no_numbers, &options(json!({"type": "Line"}))),
            Err(ChartProblem::NoValueColumn)
        );
        assert_eq!(
            ChartProblem::Unsupported("Pie".into()).to_string(),
            "A Pie chart cannot be drawn here."
        );
    }

    #[test]
    fn a_new_chart_starts_as_the_type_the_columns_suggest() {
        let columns = |columns: &[(&str, &str)]| suggest_chart_type(&table(columns, Vec::new()));
        assert_eq!(
            columns(&[("T", "datetime"), ("N", "long")]),
            ChartType::TimeLine
        );
        assert_eq!(
            columns(&[("S", "string"), ("N", "real")]),
            ChartType::Column
        );
        assert_eq!(columns(&[("A", "long"), ("B", "long")]), ChartType::Line);
        assert_eq!(columns(&[("S", "string")]), ChartType::Column);
    }

    #[test]
    fn datetimes_and_timespans_are_read_to_milliseconds() {
        assert_eq!(parse_datetime_text("1970-01-01T00:00:01Z"), Some(1000.0));
        assert_eq!(
            parse_datetime_text("1970-01-01T00:00:00.1234567Z"),
            Some(123.4567)
        );
        assert_eq!(parse_datetime_text("1970-01-01T01:00:00+01:00"), Some(0.0));
        assert_eq!(parse_datetime_text("yesterday"), None);
        assert_eq!(parse_timespan_text("00:00:01.5000000"), Some(1500.0));
        assert_eq!(parse_timespan_text("1.02:03:04"), Some(93_784_000.0));
        assert_eq!(parse_timespan_text("-00:01:00"), Some(-60_000.0));
        assert_eq!(parse_timespan_text("12:00"), None);
        assert_eq!(parse_timespan_text("a:b:c"), None);
    }
}
