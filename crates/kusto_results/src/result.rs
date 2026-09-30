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
    pub tables: Vec<Table>,
    pub table_views: Vec<TableView>,
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
                "tableViews" => {
                    result.table_views =
                        parse_field::<Vec<TableView>>(raw, "tableViews")?.unwrap_or_default()
                }
                "tables" => {
                    saw_tables = true;
                    result.tables = parse_tables(raw)?;
                }
                _ => result.extra.push(ExtraProperty {
                    name,
                    value: raw.to_owned(),
                }),
            }
        }
        if !saw_tables {
            bail!("the result file has no tables property");
        }
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("could not write the result file")
    }

    /// The row count shown in the results panel badge: rows across every table.
    pub fn total_rows(&self) -> usize {
        self.tables.iter().map(|table| table.rows.len()).sum()
    }

    pub fn table_view(&self, name: &str) -> Option<&TableView> {
        self.table_views.iter().find(|view| view.name == name)
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
        if !self.table_views.is_empty() {
            map.serialize_entry("tableViews", &self.table_views)?;
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
    let columns: Vec<Column> = raw
        .columns
        .into_iter()
        .map(|column| Column::new(column.name, column.type_name))
        .collect();
    let mut rows = Vec::with_capacity(raw.rows.len());
    for (row_index, raw_row) in raw.rows.iter().enumerate() {
        if raw_row.len() != columns.len() {
            bail!(
                "table {} row {} has {} cells but the table has {} columns",
                raw.name,
                row_index + 1,
                raw_row.len(),
                columns.len()
            );
        }
        let mut row = Vec::with_capacity(columns.len());
        for (column, raw_cell) in columns.iter().zip(raw_row) {
            row.push(parse_cell(raw_cell, column.kind).with_context(|| {
                format!(
                    "table {} row {} column {}",
                    raw.name,
                    row_index + 1,
                    column.name
                )
            })?);
        }
        rows.push(row);
    }
    Ok(Table {
        name: raw.name,
        columns,
        rows,
    })
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
    fn rejects_rows_with_the_wrong_cell_count() {
        let broken =
            r#"{"tables":[{"name":"t","columns":[{"name":"a","type":"long"}],"rows":[[1,2]]}]}"#;
        let error = ResultSet::from_json(broken).unwrap_err();
        assert!(format!("{error:#}").contains("2 cells"));
    }

    #[test]
    fn rejects_a_file_without_tables() {
        assert!(ResultSet::from_json(r#"{"query":"q"}"#).is_err());
        assert!(ResultSet::from_json("[]").is_err());
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
