//! Reads the answers of the Kusto REST API: the v2 query response and its error bodies.

use anyhow::{Context as _, Result, anyhow, bail};
use kusto_results::{Column, Table};
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct Frame<'a> {
    #[serde(rename = "FrameType")]
    frame_type: String,
    #[serde(rename = "TableKind", default)]
    table_kind: Option<String>,
    #[serde(rename = "TableName", default)]
    table_name: Option<String>,
    #[serde(rename = "Columns", default)]
    columns: Option<Vec<FrameColumn>>,
    #[serde(rename = "Rows", default, borrow)]
    rows: Option<Vec<&'a RawValue>>,
    #[serde(rename = "HasErrors", default)]
    has_errors: bool,
    #[serde(rename = "Cancelled", default)]
    cancelled: bool,
    #[serde(rename = "OneApiErrors", default)]
    errors: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct FrameColumn {
    #[serde(rename = "ColumnName")]
    name: String,
    #[serde(rename = "ColumnType")]
    type_name: String,
}

/// The primary result tables of a v2 response, in the order the service sent them.
///
/// Other tables, such as query properties and completion information, are not results.
pub fn parse_query_response(body: &[u8]) -> Result<Vec<Table>> {
    let text = std::str::from_utf8(body).context("the response is not UTF-8")?;
    let frames: Vec<Frame<'_>> =
        serde_json::from_str(text).context("the response is not a list of v2 frames")?;

    let mut tables: Vec<Table> = Vec::new();
    for frame in frames {
        match frame.frame_type.as_str() {
            "DataTable" if frame.table_kind.as_deref() == Some("PrimaryResult") => {
                let name = unique_table_name(&tables, frame.table_name.unwrap_or_default());
                let columns = frame
                    .columns
                    .unwrap_or_default()
                    .into_iter()
                    .map(|column| Column::new(column.name, column.type_name))
                    .collect();
                tables.push(read_table(name, columns, frame.rows.unwrap_or_default())?);
            }
            "DataSetCompletion" => {
                if frame.has_errors {
                    return Err(completion_error(frame.errors.as_deref().unwrap_or_default()));
                }
                if frame.cancelled {
                    bail!("The query was cancelled.");
                }
            }
            _ => {}
        }
    }
    Ok(tables)
}

fn read_table(name: String, columns: Vec<Column>, rows: Vec<&RawValue>) -> Result<Table> {
    let mut cell_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let text = row.get();
        // With in-data error reporting a row can be an object holding the error that ended the table.
        if text.starts_with('{') {
            let error: Value = serde_json::from_str(text).context("a result row is invalid")?;
            let errors = error
                .get("OneApiErrors")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            return Err(completion_error(errors));
        }
        cell_rows.push(
            serde_json::from_str::<Vec<&RawValue>>(text).context("a result row is invalid")?,
        );
    }
    Table::from_raw_rows(name, columns, &cell_rows)
}

/// The service names every primary table `PrimaryResult`, but saved views are found by name.
fn unique_table_name(existing: &[Table], name: String) -> String {
    let name = if name.is_empty() {
        "PrimaryResult".to_string()
    } else {
        name
    };
    if existing.iter().all(|table| table.name != name) {
        return name;
    }
    (2..)
        .map(|number| format!("{name}_{number}"))
        .find(|candidate| existing.iter().all(|table| &table.name != candidate))
        .unwrap_or(name)
}

fn completion_error(errors: &[Value]) -> anyhow::Error {
    errors
        .first()
        .and_then(|error| error.get("error"))
        .map(error_message)
        .map(|message| anyhow!(message))
        .unwrap_or_else(|| anyhow!("The query failed."))
}

/// The message of a failed request: the status and the service's own explanation.
pub fn http_error(status: u16, body: &[u8]) -> anyhow::Error {
    let text = String::from_utf8_lossy(body);
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| value.get("error").map(error_message))
        .unwrap_or_else(|| text.trim().chars().take(2000).collect());
    match (status, message.is_empty()) {
        (401, true) => anyhow!("Kusto rejected the access token (HTTP 401)."),
        (_, true) => anyhow!("The request failed (HTTP {status})."),
        (401, false) => anyhow!("Kusto rejected the access token (HTTP 401): {message}"),
        (_, false) => anyhow!("{message}"),
    }
}

/// The most specific message in an error object, which is the innermost one: the outer ones say
/// only that the request was bad.
fn error_message(error: &Value) -> String {
    let mut deepest = error;
    while let Some(inner) = deepest.get("innererror").filter(|inner| inner.is_object()) {
        deepest = inner;
    }
    [deepest, error]
        .into_iter()
        .flat_map(|candidate| ["@message", "message"].map(|key| candidate.get(key)))
        .flatten()
        .filter_map(Value::as_str)
        .find(|message| !message.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use kusto_results::Cell;

    use super::*;

    const RESPONSE: &str = r#"[
        {"FrameType":"DataSetHeader","IsProgressive":false,"Version":"v2.0"},
        {"FrameType":"DataTable","TableId":0,"TableKind":"QueryProperties","TableName":"@ExtendedProperties",
         "Columns":[{"ColumnName":"Key","ColumnType":"string"}],"Rows":[["Visualization"]]},
        {"FrameType":"DataTable","TableId":1,"TableKind":"PrimaryResult","TableName":"PrimaryResult",
         "Columns":[{"ColumnName":"a","ColumnType":"long"},{"ColumnName":"d","ColumnType":"dynamic"}],
         "Rows":[[9223372036854775807,{"k":[1,2]}],[null,null]]},
        {"FrameType":"DataTable","TableId":2,"TableKind":"PrimaryResult","TableName":"PrimaryResult",
         "Columns":[{"ColumnName":"b","ColumnType":"real"}],"Rows":[[5.0]]},
        {"FrameType":"DataTable","TableId":3,"TableKind":"QueryCompletionInformation","TableName":"QueryCompletionInformation",
         "Columns":[{"ColumnName":"Severity","ColumnType":"int"}],"Rows":[[4]]},
        {"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":false}
    ]"#;

    #[test]
    fn keeps_only_primary_results_with_unique_names() {
        let tables = parse_query_response(RESPONSE.as_bytes()).unwrap();
        let names: Vec<_> = tables.iter().map(|table| table.name.as_str()).collect();
        assert_eq!(names, ["PrimaryResult", "PrimaryResult_2"]);
        assert_eq!(tables[0].columns[0].type_name, "long");
    }

    #[test]
    fn reads_values_without_losing_digits() {
        let tables = parse_query_response(RESPONSE.as_bytes()).unwrap();
        assert_eq!(tables[0].rows[0][0], Cell::Int(i64::MAX));
        assert_eq!(tables[0].rows[0][1].display_text(), r#"{"k":[1,2]}"#);
        assert_eq!(tables[0].rows[1][0], Cell::Null);
        assert_eq!(tables[1].rows[0][0].display_text(), "5.0");
    }

    #[test]
    fn a_query_without_rows_still_has_its_columns() {
        let body = r#"[{"FrameType":"DataTable","TableKind":"PrimaryResult","TableName":"PrimaryResult",
            "Columns":[{"ColumnName":"a","ColumnType":"long"}],"Rows":[]}]"#;
        let tables = parse_query_response(body.as_bytes()).unwrap();
        assert_eq!(tables[0].columns.len(), 1);
        assert!(tables[0].rows.is_empty());
    }

    #[test]
    fn a_completion_with_errors_fails_the_query() {
        let body = r#"[{"FrameType":"DataSetCompletion","HasErrors":true,"Cancelled":false,
            "OneApiErrors":[{"error":{"code":"LimitsExceeded","message":"outer",
            "innererror":{"message":"Query result set has exceeded the internal data size limit"}}}]}]"#;
        let error = parse_query_response(body.as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Query result set has exceeded the internal data size limit"
        );
    }

    #[test]
    fn an_error_row_fails_the_query() {
        let body = r#"[{"FrameType":"DataTable","TableKind":"PrimaryResult","TableName":"PrimaryResult",
            "Columns":[{"ColumnName":"a","ColumnType":"long"}],
            "Rows":[[1],{"OneApiErrors":[{"error":{"message":"Partial query failure"}}]}]}]"#;
        let error = parse_query_response(body.as_bytes()).unwrap_err();
        assert_eq!(error.to_string(), "Partial query failure");
    }

    #[test]
    fn a_cancelled_completion_fails_the_query() {
        let body = r#"[{"FrameType":"DataSetCompletion","HasErrors":false,"Cancelled":true}]"#;
        let error = parse_query_response(body.as_bytes()).unwrap_err();
        assert_eq!(error.to_string(), "The query was cancelled.");
    }

    #[test]
    fn rejects_rows_of_the_wrong_width() {
        let body = r#"[{"FrameType":"DataTable","TableKind":"PrimaryResult","TableName":"t",
            "Columns":[{"ColumnName":"a","ColumnType":"long"}],"Rows":[[1,2]]}]"#;
        assert!(parse_query_response(body.as_bytes()).is_err());
    }

    #[test]
    fn rejects_a_body_that_is_not_frames() {
        assert!(parse_query_response(b"{}").is_err());
        assert!(parse_query_response(b"plain text").is_err());
    }

    #[test]
    fn an_http_error_shows_the_innermost_message() {
        let body = r#"{"error":{"code":"General_BadRequest","message":"Request is invalid and cannot be executed.",
            "@message":"Request is invalid and cannot be processed: Semantic error: SEM0100: bad",
            "innererror":{"code":"SEM0100","message":"'take' operator: Failed to resolve table",
            "@message":"Semantic error: SEM0100: 'take' operator: Failed to resolve table"}}}"#;
        assert_eq!(
            http_error(400, body.as_bytes()).to_string(),
            "Semantic error: SEM0100: 'take' operator: Failed to resolve table"
        );
    }

    #[test]
    fn an_http_error_without_json_shows_the_text() {
        assert_eq!(
            http_error(502, b"Bad gateway\n").to_string(),
            "Bad gateway"
        );
        assert_eq!(
            http_error(500, b"").to_string(),
            "The request failed (HTTP 500)."
        );
        assert!(
            http_error(401, b"")
                .to_string()
                .contains("rejected the access token")
        );
    }
}
