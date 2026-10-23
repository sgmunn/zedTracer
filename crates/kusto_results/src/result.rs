//! The result set model and the `.ktt` file format.
//!
//! A `.ktt` file is the JSON document the KustoTraceTools VS Code extension writes (legacy
//! `.kqr` files use the same format). Reading keeps every value exactly as the server sent
//! it: integers stay 64-bit, decimals keep their full digits, dynamic values keep their key
//! order, and properties this crate does not know (charts, for example) are written back
//! untouched.

use std::borrow::Cow;

use anyhow::{Context as _, Result, bail};
use serde::de::{MapAccess, Visitor};
use serde::ser::{Error as _, SerializeMap, SerializeStruct};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::chart::Chart;
use crate::statistics::QueryStatistics;

/// The Kusto type of a column, reduced to the distinctions sorting and filtering need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    Bool,
    Int,
    Long,
    Real,
    Decimal,
    String,
    DateTime,
    TimeSpan,
    Guid,
    Dynamic,
    /// A type name this crate does not recognise; treated like a string.
    Other,
}

impl ColumnKind {
    pub fn from_type_name(type_name: &str) -> Self {
        match type_name.to_ascii_lowercase().as_str() {
            "bool" | "boolean" => ColumnKind::Bool,
            "int" => ColumnKind::Int,
            "long" => ColumnKind::Long,
            "real" | "double" => ColumnKind::Real,
            "decimal" => ColumnKind::Decimal,
            "string" => ColumnKind::String,
            "datetime" | "date" => ColumnKind::DateTime,
            "timespan" => ColumnKind::TimeSpan,
            "guid" => ColumnKind::Guid,
            "dynamic" => ColumnKind::Dynamic,
            _ => ColumnKind::Other,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ColumnKind::Int | ColumnKind::Long | ColumnKind::Real | ColumnKind::Decimal
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    /// The type name exactly as the file spelled it, so it can be written back unchanged.
    pub type_name: String,
    pub kind: ColumnKind,
}

impl Column {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        let type_name = type_name.into();
        let kind = ColumnKind::from_type_name(&type_name);
        Self {
            name: name.into(),
            type_name,
            kind,
        }
    }
}

/// One value of one row.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Bool(bool),
    /// An integer literal. Also used in `real` columns when the server sent `5` rather than `5.0`.
    Int(i64),
    Real(f64),
    /// The number's original text, kept so a value beyond 64 bits survives a round trip.
    Decimal(String),
    /// string, datetime, timespan and guid values, as sent.
    Text(String),
    /// An object or array held in a dynamic column. Boxed so the common cells stay small.
    Dynamic(Box<Value>),
}

impl Cell {
    /// The text the grid shows, the search box matches and copy writes.
    ///
    /// Null is empty, objects and arrays are compact JSON, and numbers use the form they were
    /// read in (so `5.0` stays `5.0`).
    pub fn display_text(&self) -> Cow<'_, str> {
        match self {
            Cell::Null => Cow::Borrowed(""),
            Cell::Bool(true) => Cow::Borrowed("true"),
            Cell::Bool(false) => Cow::Borrowed("false"),
            Cell::Int(value) => Cow::Owned(value.to_string()),
            Cell::Real(value) => Cow::Owned(
                serde_json::Number::from_f64(*value)
                    .map_or_else(|| value.to_string(), |number| number.to_string()),
            ),
            Cell::Decimal(text) | Cell::Text(text) => Cow::Borrowed(text),
            Cell::Dynamic(value) => Cow::Owned(value.to_string()),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }
}

impl Serialize for Cell {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Cell::Null => serializer.serialize_none(),
            Cell::Bool(value) => serializer.serialize_bool(*value),
            Cell::Int(value) => serializer.serialize_i64(*value),
            Cell::Real(value) => serializer.serialize_f64(*value),
            Cell::Decimal(text) => RawValue::from_string(text.clone())
                .map_err(S::Error::custom)?
                .serialize(serializer),
            Cell::Text(text) => serializer.serialize_str(text),
            Cell::Dynamic(value) => value.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
}

impl Table {
    /// Builds a table from rows whose cells are still JSON text, reading each cell by the kind of
    /// its column. This is how a server response becomes a table without losing any digits.
    pub fn from_raw_rows(
        name: String,
        columns: Vec<Column>,
        raw_rows: &[Vec<&RawValue>],
    ) -> Result<Self> {
        let mut rows = Vec::with_capacity(raw_rows.len());
        for (row_index, raw_row) in raw_rows.iter().enumerate() {
            if raw_row.len() != columns.len() {
                bail!(
                    "table {} row {} has {} cells but the table has {} columns",
                    name,
                    row_index + 1,
                    raw_row.len(),
                    columns.len()
                );
            }
            let mut row = Vec::with_capacity(columns.len());
            for (column, raw_cell) in columns.iter().zip(raw_row) {
                row.push(parse_cell(raw_cell, column.kind).with_context(|| {
                    format!("table {} row {} column {}", name, row_index + 1, column.name)
                })?);
            }
            rows.push(row);
        }
        Ok(Table {
            name,
            columns,
            rows,
        })
    }

    /// Finds a column by name, preferring an exact match over a case-insensitive one.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.name == name)
            .or_else(|| {
                self.columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(name))
            })
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        self.rows.get(row)?.get(column)
    }
}

/// A column's saved position and width. `index` refers to the table's original column order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnLayout {
    pub index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

/// Saved presentation of one table, or of a derived view of it such as the structured grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gutter_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<ColumnLayout>>,
}

/// A top-level property this crate does not interpret, kept as raw JSON.
#[derive(Debug, Clone)]
pub struct ExtraProperty {
    pub name: String,
    pub value: Box<RawValue>,
}

impl PartialEq for ExtraProperty {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.value.get() == other.value.get()
    }
}

/// A result file that is valid JSON but holds no `tables`, so there is nothing to show.
#[derive(Debug)]
pub struct NoResultData;

impl std::fmt::Display for NoResultData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the result file has no tables property")
    }
}

impl std::error::Error for NoResultData {}

/// Everything one query run returned, plus what was saved with it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResultSet {
    pub query: Option<String>,
    pub cluster: Option<String>,
    pub database: Option<String>,
    pub parameters: Option<Map<String, Value>>,
    pub execution_started_at: Option<String>,
    pub execution_duration_ms: Option<u64>,
    pub client_request_id: Option<String>,
    /// What the run cost the service, as it sent it. See [`QueryStatistics`].
    pub statistics: Option<Value>,
    pub tables: Vec<Table>,
    pub table_views: Vec<TableView>,
    pub charts: Vec<Chart>,
    /// Properties this crate does not interpret, in file order, written back verbatim.
    pub extra: Vec<ExtraProperty>,
}

impl ResultSet {
    pub fn from_json(text: &str) -> Result<Self> {
        let fields: FieldList<'_> =
            serde_json::from_str(text).context("the result file is not a JSON object")?;
        let mut result = ResultSet::default();
        let mut saw_tables = false;
        for (name, raw) in fields.0 {
            match name.as_str() {
                "query" => result.query = parse_field(raw, "query")?,
                "cluster" => result.cluster = parse_field(raw, "cluster")?,
                "database" => result.database = parse_field(raw, "database")?,
                "parameters" => result.parameters = parse_field(raw, "parameters")?,
                "executionStartedAt" => {
                    result.execution_started_at = parse_field(raw, "executionStartedAt")?
                }
                "executionDurationMs" => {
                    result.execution_duration_ms = parse_field(raw, "executionDurationMs")?
                }
                "clientRequestId" => {
                    result.client_request_id = parse_field(raw, "clientRequestId")?
                }
                "queryStatistics" => result.statistics = parse_field(raw, "queryStatistics")?,
                "tableViews" => {
                    result.table_views =
                        parse_field::<Vec<TableView>>(raw, "tableViews")?.unwrap_or_default()
                }
                "tables" => {
                    saw_tables = true;
                    result.tables = parse_tables(raw)?;
                }
                "charts" => match parse_charts(raw) {
                    Some(charts) => result.charts = charts,
                    None => result.extra.push(ExtraProperty {
                        name,
                        value: raw.to_owned(),
                    }),
                },
                _ => result.extra.push(ExtraProperty {
                    name,
                    value: raw.to_owned(),
                }),
            }
        }
        if !saw_tables {
            return Err(NoResultData.into());
        }
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("could not write the result file")
    }

    pub fn query_statistics(&self) -> Option<QueryStatistics> {
        self.statistics.as_ref().map(QueryStatistics::from_payload)
    }

    /// The row count shown in the results panel badge: rows across every table.
    pub fn total_rows(&self) -> usize {
        self.tables.iter().map(|table| table.rows.len()).sum()
    }

    pub fn table_view(&self, name: &str) -> Option<&TableView> {
        self.table_views.iter().find(|view| view.name == name)
    }

    /// The table a chart reads: the one it names, else the first.
    pub fn chart_table(&self, chart: &Chart) -> Option<&Table> {
        match chart.table_name.as_deref() {
            Some(name) => self.tables.iter().find(|table| table.name == name),
            None => self.tables.first(),
        }
    }

    /// Replaces the charts. A `charts` property that could not be read as charts is dropped,
    /// because it would otherwise be written next to the new ones.
    pub fn set_charts(&mut self, charts: Vec<Chart>) {
        self.charts = charts;
        self.extra.retain(|property| property.name != "charts");
    }

    /// Stores a view, replacing the saved one with the same name.
    pub fn set_table_view(&mut self, view: TableView) {
        match self
            .table_views
            .iter_mut()
            .find(|existing| existing.name == view.name)
        {
            Some(existing) => *existing = view,
            None => self.table_views.push(view),
        }
    }

    /// This result with the data of a newer run of its query. What the file holds for itself
    /// stays: the query as it was stored, where it ran, the values of its parameters, its charts
    /// and the properties this crate does not interpret. A saved layout stays only while its
    /// table has the columns it was made for, because a layout refers to columns by position.
    pub fn refreshed_with(&self, run: ResultSet) -> ResultSet {
        let table_views = self
            .table_views
            .iter()
            .filter(|view| {
                let table_name = view
                    .name
                    .split_once("::")
                    .map_or(view.name.as_str(), |(table_name, _)| table_name);
                column_names(&run.tables, table_name)
                    .is_some_and(|fresh| column_names(&self.tables, table_name) == Some(fresh))
            })
            .cloned()
            .collect();
        ResultSet {
            query: self.query.clone(),
            cluster: self.cluster.clone(),
            database: self.database.clone(),
            parameters: self.parameters.clone(),
            execution_started_at: run.execution_started_at,
            execution_duration_ms: run.execution_duration_ms,
            client_request_id: run.client_request_id,
            statistics: run.statistics,
            tables: run.tables,
            table_views,
            charts: self.charts.clone(),
            extra: self.extra.clone(),
        }
    }
}

fn column_names<'a>(tables: &'a [Table], table_name: &str) -> Option<Vec<&'a str>> {
    let table = tables.iter().find(|table| table.name == table_name)?;
    Some(
        table
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect(),
    )
}

impl Serialize for ResultSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if let Some(query) = &self.query {
            map.serialize_entry("query", query)?;
        }
        if let Some(cluster) = &self.cluster {
            map.serialize_entry("cluster", cluster)?;
        }
        if let Some(database) = &self.database {
            map.serialize_entry("database", database)?;
        }
        if let Some(parameters) = &self.parameters {
            map.serialize_entry("parameters", parameters)?;
        }
        map.serialize_entry("tables", &TablesOut(&self.tables))?;
        if let Some(started) = &self.execution_started_at {
            map.serialize_entry("executionStartedAt", started)?;
        }
        if let Some(duration) = &self.execution_duration_ms {
            map.serialize_entry("executionDurationMs", duration)?;
        }
        if let Some(request_id) = &self.client_request_id {
            map.serialize_entry("clientRequestId", request_id)?;
        }
        if let Some(statistics) = &self.statistics {
            map.serialize_entry("queryStatistics", statistics)?;
        }
        if !self.table_views.is_empty() {
            map.serialize_entry("tableViews", &self.table_views)?;
        }
        if !self.charts.is_empty() {
            map.serialize_entry("charts", &self.charts)?;
        }
        for property in &self.extra {
            map.serialize_entry(&property.name, &property.value)?;
        }
        map.end()
    }
}

struct TablesOut<'a>(&'a [Table]);

impl Serialize for TablesOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}

impl Serialize for Table {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Table", 3)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("columns", &ColumnsOut(&self.columns))?;
        state.serialize_field("rows", &self.rows)?;
        state.end()
    }
}

struct ColumnsOut<'a>(&'a [Column]);

impl Serialize for ColumnsOut<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct ColumnOut<'a> {
            name: &'a str,
            #[serde(rename = "type")]
            type_name: &'a str,
        }
        serializer.collect_seq(self.0.iter().map(|column| ColumnOut {
            name: &column.name,
            type_name: &column.type_name,
        }))
    }
}

/// The top-level properties of the file, in order, each still as raw JSON text.
struct FieldList<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for FieldList<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldVisitor;

        impl<'de> Visitor<'de> for FieldVisitor {
            type Value = FieldList<'de>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = Vec::new();
                while let Some(name) = map.next_key::<String>()? {
                    fields.push((name, map.next_value::<&'de RawValue>()?));
                }
                Ok(FieldList(fields))
            }
        }

        deserializer.deserialize_map(FieldVisitor)
    }
}

fn parse_charts(raw: &RawValue) -> Option<Vec<Chart>> {
    let value: Value = serde_json::from_str(raw.get()).ok()?;
    Chart::list_from_value(value)
}

fn parse_field<'a, T: Deserialize<'a>>(raw: &'a RawValue, name: &str) -> Result<Option<T>> {
    serde_json::from_str(raw.get()).with_context(|| format!("the {name} property is invalid"))
}

#[derive(Deserialize)]
struct RawColumn {
    name: String,
    #[serde(rename = "type")]
    type_name: String,
}

#[derive(Deserialize)]
struct RawTable<'a> {
    name: String,
    columns: Vec<RawColumn>,
    #[serde(borrow)]
    rows: Vec<Vec<&'a RawValue>>,
}

fn parse_tables(raw: &RawValue) -> Result<Vec<Table>> {
    let raw_tables: Vec<RawTable<'_>> =
        serde_json::from_str(raw.get()).context("the tables property is invalid")?;
    raw_tables.into_iter().map(parse_table).collect()
}

fn parse_table(raw: RawTable<'_>) -> Result<Table> {
    let columns = raw
        .columns
        .into_iter()
        .map(|column| Column::new(column.name, column.type_name))
        .collect();
    Table::from_raw_rows(raw.name, columns, &raw.rows)
}

fn parse_cell(raw: &RawValue, kind: ColumnKind) -> Result<Cell> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'n') => Ok(Cell::Null),
        Some(b't') => Ok(Cell::Bool(true)),
        Some(b'f') => Ok(Cell::Bool(false)),
        Some(b'"') => Ok(Cell::Text(serde_json::from_str(text)?)),
        Some(b'{') | Some(b'[') => Ok(Cell::Dynamic(Box::new(serde_json::from_str(text)?))),
        Some(_) if kind == ColumnKind::Decimal => Ok(Cell::Decimal(text.to_string())),
        Some(_) => match text.parse::<i64>() {
            Ok(value) => Ok(Cell::Int(value)),
            Err(_) => match text.parse::<f64>() {
                Ok(value) if value.is_finite() => Ok(Cell::Real(value)),
                _ => Ok(Cell::Decimal(text.to_string())),
            },
        },
        None => bail!("empty cell"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "query": "q",
      "cluster": "c",
      "database": "d",
      "parameters": {"raid": "abc"},
      "tables": [{
        "name": "PrimaryResult",
        "columns": [{"name": "Big", "type": "long"}, {"name": "Real", "type": "real"},
                    {"name": "Money", "type": "decimal"}, {"name": "Payload", "type": "dynamic"},
                    {"name": "Note", "type": "string"}],
        "rows": [
          [9223372036854775807, 5.0, 79228162514264337593543950335, {"b": 1, "a": [1, 2]}, "x"],
          [-9223372036854775808, 5, 0.10, null, null]
        ]
      }],
      "executionStartedAt": "2026-01-01T00:00:00.000Z",
      "executionDurationMs": 12,
      "clientRequestId": "id",
      "charts": [{"name": "kept", "options": {"type": "linechart"}}]
    }"#;

    #[test]
    fn reads_values_exactly() {
        let result = ResultSet::from_json(SAMPLE).unwrap();
        let rows = &result.tables[0].rows;
        assert_eq!(rows[0][0], Cell::Int(i64::MAX));
        assert_eq!(rows[1][0], Cell::Int(i64::MIN));
        assert_eq!(rows[0][1], Cell::Real(5.0));
        assert_eq!(rows[1][1], Cell::Int(5));
        assert_eq!(
            rows[0][2],
            Cell::Decimal("79228162514264337593543950335".into())
        );
        assert_eq!(rows[1][2], Cell::Decimal("0.10".into()));
        assert_eq!(rows[1][3], Cell::Null);
        assert_eq!(result.total_rows(), 2);
    }

    #[test]
    fn dynamic_values_keep_key_order() {
        let result = ResultSet::from_json(SAMPLE).unwrap();
        assert_eq!(
            result.tables[0].rows[0][3].display_text(),
            r#"{"b":1,"a":[1,2]}"#
        );
    }

    #[test]
    fn display_text_uses_the_source_form() {
        let result = ResultSet::from_json(SAMPLE).unwrap();
        let rows = &result.tables[0].rows;
        assert_eq!(rows[0][1].display_text(), "5.0");
        assert_eq!(rows[1][1].display_text(), "5");
        assert_eq!(rows[1][4].display_text(), "");
    }

    #[test]
    fn round_trip_keeps_numbers_and_unknown_properties() {
        let result = ResultSet::from_json(SAMPLE).unwrap();
        let written = result.to_json().unwrap();
        assert!(written.contains("79228162514264337593543950335"));
        assert!(written.contains("9223372036854775807"));
        assert!(written.contains(r#""charts""#));
        let reread = ResultSet::from_json(&written).unwrap();
        assert_eq!(reread, result);
    }

    #[test]
    fn statistics_are_kept_as_the_service_sent_them() {
        let text = r#"{"tables":[],"queryStatistics":{"ExecutionTime":28.8199127,
            "resource_usage":{"memory":{"peak_per_node":1050944},"zzz":1,"aaa":2}}}"#;
        let result = ResultSet::from_json(text).unwrap();
        let statistics = result.query_statistics().unwrap();
        assert_eq!(statistics.execution_seconds, Some(28.8199127));
        assert_eq!(statistics.usage.memory_peak_per_node, Some(1050944));

        let written = result.to_json().unwrap();
        assert!(
            written.find(r#""zzz""#) < written.find(r#""aaa""#),
            "keys keep the order the service sent them in"
        );
        assert_eq!(ResultSet::from_json(&written).unwrap(), result);
        assert_eq!(ResultSet::default().query_statistics(), None);
    }

    #[test]
    fn rejects_rows_with_the_wrong_cell_count() {
        let broken =
            r#"{"tables":[{"name":"t","columns":[{"name":"a","type":"long"}],"rows":[[1,2]]}]}"#;
        let error = ResultSet::from_json(broken).unwrap_err();
        assert!(format!("{error:#}").contains("2 cells"));
    }

    #[test]
    fn rejects_a_file_without_tables() {
        let no_tables = ResultSet::from_json(r#"{"query":"q"}"#).unwrap_err();
        assert!(no_tables.is::<NoResultData>());
        let not_an_object = ResultSet::from_json("[]").unwrap_err();
        assert!(!not_an_object.is::<NoResultData>());
    }

    #[test]
    fn table_views_are_replaced_by_name() {
        let mut result = ResultSet::from_json(SAMPLE).unwrap();
        let view = |width| TableView {
            name: "PrimaryResult".into(),
            gutter_width: Some(64),
            columns: Some(vec![ColumnLayout {
                index: 1,
                width: Some(width),
            }]),
        };
        result.set_table_view(view(100));
        result.set_table_view(view(120));
        assert_eq!(result.table_views.len(), 1);
        assert_eq!(result.table_view("PrimaryResult"), Some(&view(120)));
    }

    const STORED: &str = r#"{
      "query": "StormEvents | take 2",
      "cluster": "https://help.kusto.windows.net",
      "database": "Samples",
      "parameters": {"raid": "abc"},
      "tables": [{"name": "PrimaryResult",
                  "columns": [{"name": "n", "type": "long"}, {"name": "label", "type": "string"}],
                  "rows": [[1, "a"], [2, "b"]]}],
      "executionStartedAt": "2026-01-01T00:00:00.000Z",
      "executionDurationMs": 12,
      "clientRequestId": "old",
      "queryStatistics": {"ExecutionTime": 1.5},
      "tableViews": [
        {"name": "PrimaryResult", "gutterWidth": 64, "columns": [{"index": 1, "width": 90}]},
        {"name": "PrimaryResult::activity-structured:0", "columns": [{"index": 0}]},
        {"name": "Gone", "columns": [{"index": 0}]}],
      "charts": [{"name": "kept", "options": {"type": "Line", "title": "T"}}],
      "later": 1
    }"#;

    fn run_of(columns: &str, rows: &str) -> ResultSet {
        ResultSet::from_json(&format!(
            r#"{{"query": "new text", "cluster": "elsewhere", "database": "Other",
                "tables": [{{"name": "PrimaryResult", "columns": {columns}, "rows": {rows}}}],
                "executionStartedAt": "2026-02-02T00:00:00.000Z",
                "executionDurationMs": 99, "clientRequestId": "new",
                "queryStatistics": {{"ExecutionTime": 0.25}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_refresh_takes_the_data_and_keeps_what_the_file_owns() {
        let stored = ResultSet::from_json(STORED).unwrap();
        let run = run_of(
            r#"[{"name": "n", "type": "long"}, {"name": "label", "type": "string"}]"#,
            r#"[[7, "x"], [8, "y"], [9, "z"]]"#,
        );
        let refreshed = stored.refreshed_with(run);

        assert_eq!(refreshed.tables[0].rows.len(), 3);
        assert_eq!(
            refreshed.execution_started_at.as_deref(),
            Some("2026-02-02T00:00:00.000Z")
        );
        assert_eq!(refreshed.execution_duration_ms, Some(99));
        assert_eq!(refreshed.client_request_id.as_deref(), Some("new"));
        assert_eq!(
            refreshed
                .query_statistics()
                .and_then(|statistics| statistics.execution_seconds),
            Some(0.25),
            "the statistics are those of the run that made the data"
        );

        assert_eq!(
            refreshed.query, stored.query,
            "the stored query is the one to repeat"
        );
        assert_eq!(refreshed.cluster, stored.cluster);
        assert_eq!(refreshed.database, stored.database);
        assert_eq!(refreshed.parameters, stored.parameters);
        assert_eq!(refreshed.charts, stored.charts);
        assert_eq!(refreshed.extra, stored.extra);

        let written = refreshed.to_json().unwrap();
        assert_eq!(ResultSet::from_json(&written).unwrap(), refreshed);
    }

    #[test]
    fn a_layout_stays_while_its_table_has_the_same_columns() {
        let stored = ResultSet::from_json(STORED).unwrap();
        let same_columns = stored.refreshed_with(run_of(
            r#"[{"name": "n", "type": "long"}, {"name": "label", "type": "string"}]"#,
            "[]",
        ));
        let names: Vec<&str> = same_columns
            .table_views
            .iter()
            .map(|view| view.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["PrimaryResult", "PrimaryResult::activity-structured:0"],
            "a view of a table the run did not return is dropped, one derived from a kept table stays"
        );

        let other_columns = stored.refreshed_with(run_of(
            r#"[{"name": "label", "type": "string"}, {"name": "n", "type": "long"}]"#,
            "[]",
        ));
        assert!(
            other_columns.table_views.is_empty(),
            "a layout made for other columns would put widths on the wrong ones"
        );
    }

    #[test]
    fn charts_are_read_and_written_between_the_known_properties_and_the_extras() {
        let text = r#"{"tables":[{"name":"t","columns":[{"name":"a","type":"long"}],"rows":[[1]]}],
            "later":1,
            "charts":[{"options":{"type":"Column","title":"T"}},
                      {"name":"n","tableName":"t","options":{"type":"Line","unknown":[1]}}]}"#;
        let result = ResultSet::from_json(text).unwrap();
        assert_eq!(result.charts.len(), 2);
        assert_eq!(result.charts[0].options.title.as_deref(), Some("T"));
        assert!(
            result
                .extra
                .iter()
                .all(|property| property.name != "charts")
        );

        let written = result.to_json().unwrap();
        let reread = ResultSet::from_json(&written).unwrap();
        assert_eq!(reread, result);
        assert!(written.find(r#""charts""#) < written.find(r#""later""#));
    }

    #[test]
    fn charts_that_cannot_be_read_are_written_back_untouched() {
        let text = r#"{"tables":[{"name":"t","columns":[{"name":"a","type":"long"}],"rows":[[1]]}],
            "charts":[{"name":"no options"}]}"#;
        let result = ResultSet::from_json(text).unwrap();
        assert!(result.charts.is_empty());
        let written = result.to_json().unwrap();
        assert!(written.contains(r#""no options""#));
        assert_eq!(ResultSet::from_json(&written).unwrap(), result);
    }

    #[test]
    fn a_chart_reads_the_table_it_names_else_the_first() {
        let text = r#"{"tables":[
            {"name":"a","columns":[{"name":"x","type":"long"}],"rows":[[1]]},
            {"name":"b","columns":[{"name":"x","type":"long"}],"rows":[[2]]}]}"#;
        let result = ResultSet::from_json(text).unwrap();
        let chart = |table_name: Option<&str>| Chart {
            table_name: table_name.map(str::to_string),
            ..Chart::default()
        };
        let name_of = |table_name| {
            result
                .chart_table(&chart(table_name))
                .map(|table| table.name.clone())
        };
        assert_eq!(name_of(None).as_deref(), Some("a"));
        assert_eq!(name_of(Some("b")).as_deref(), Some("b"));
        assert_eq!(name_of(Some("gone")), None);
    }

    #[test]
    fn setting_charts_replaces_charts_that_could_not_be_read() {
        let text = r#"{"tables":[{"name":"t","columns":[{"name":"a","type":"long"}],"rows":[[1]]}],
            "charts":"junk"}"#;
        let mut result = ResultSet::from_json(text).unwrap();
        result.set_charts(vec![Chart::new(crate::chart::ChartOptions::new(
            crate::chart::ChartType::Bar,
        ))]);
        let written = result.to_json().unwrap();
        assert_eq!(written.matches(r#""charts""#).count(), 1);
        assert!(!written.contains("junk"));

        result.set_charts(Vec::new());
        assert!(!result.to_json().unwrap().contains(r#""charts""#));
    }

    #[test]
    fn column_lookup_prefers_exact_then_case_insensitive() {
        let table = Table {
            name: "t".into(),
            columns: vec![Column::new("level", "long"), Column::new("Level", "string")],
            rows: Vec::new(),
        };
        assert_eq!(table.column_index("Level"), Some(1));
        assert_eq!(table.column_index("LEVEL"), Some(0));
        assert_eq!(table.column_index("missing"), None);
    }
}
