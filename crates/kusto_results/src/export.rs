//! Text forms of a table selection for the clipboard: plain text (tab-separated), Markdown,
//! HTML and a KQL `datatable` expression.
//!
//! Every function takes the source rows and columns to write, in the order to write them, so
//! the caller decides between a selection, the current view or the whole table. Values are
//! written as the source data, not as the inspector displays them.

use crate::result::{Cell, ColumnKind, Table};

const HTML_TABLE_ATTRIBUTES: &str =
    r#"border="1" style="border-collapse: collapse; width: fit-content;""#;
const HTML_HEADER_ATTRIBUTES: &str = r#"style="padding: 4px; font-weight: bold;""#;
const HTML_CELL_ATTRIBUTES: &str =
    r#"style="padding: 4px; white-space: nowrap; overflow-x: auto; max-width: 500px;""#;

fn cell_text(table: &Table, row: usize, column: usize) -> String {
    table
        .cell(row, column)
        .map(|cell| cell.display_text().into_owned())
        .unwrap_or_default()
}

fn column_name(table: &Table, column: usize) -> &str {
    table
        .columns
        .get(column)
        .map_or("", |column| column.name.as_str())
}

/// Tab-separated text with a header row. A value holding a tab, a line break or a double quote
/// is wrapped in double quotes with inner quotes doubled, as spreadsheets expect.
pub fn tsv(table: &Table, rows: &[usize], columns: &[usize]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(
        columns
            .iter()
            .map(|column| escape_tsv(column_name(table, *column)))
            .collect::<Vec<_>>()
            .join("\t"),
    );
    for row in rows {
        lines.push(
            columns
                .iter()
                .map(|column| escape_tsv(&cell_text(table, *row, *column)))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.join("\n")
}

fn escape_tsv(value: &str) -> String {
    if value.contains(['\t', '\r', '\n', '"']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// What Copy puts on the clipboard as plain text: one cell alone is its raw value, with no
/// header, quoting or separators; anything larger is tab-separated text.
pub fn copy_text(table: &Table, rows: &[usize], columns: &[usize]) -> String {
    match (rows, columns) {
        ([row], [column]) => cell_text(table, *row, *column),
        _ => tsv(table, rows, columns),
    }
}

/// A Markdown pipe table. Pipes are escaped and line breaks flattened so a value cannot break
/// the table.
pub fn markdown(table: &Table, rows: &[usize], columns: &[usize]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let line = |cells: Vec<String>| format!("| {} |", cells.join(" | "));
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(line(
        columns
            .iter()
            .map(|column| escape_markdown(column_name(table, *column)))
            .collect(),
    ));
    lines.push(line(columns.iter().map(|_| "---".to_string()).collect()));
    for row in rows {
        lines.push(line(
            columns
                .iter()
                .map(|column| escape_markdown(&cell_text(table, *row, *column)))
                .collect(),
        ));
    }
    lines.join("\n")
}

fn escape_markdown(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', "")
}

/// An HTML table with the same inline styling the VS Code extension uses.
pub fn html(table: &Table, rows: &[usize], columns: &[usize]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let mut output = format!("<table {HTML_TABLE_ATTRIBUTES}><thead><tr>");
    for column in columns {
        output.push_str(&format!(
            "<th {HTML_HEADER_ATTRIBUTES}>{}</th>",
            escape_html(column_name(table, *column))
        ));
    }
    output.push_str("</tr></thead><tbody>");
    for row in rows {
        output.push_str("<tr>");
        for column in columns {
            output.push_str(&format!(
                "<td {HTML_CELL_ATTRIBUTES}>{}</td>",
                escape_html(&cell_text(table, *row, *column))
            ));
        }
        output.push_str("</tr>");
    }
    output.push_str("</tbody></table>");
    output
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Names Kusto reads as keywords, so a column called one of these is written in brackets.
/// Taken from Kusto.Language 12.4.0 (`KustoFacts.BracketNameIfNecessary`); matching is exact
/// and case-sensitive, so `Where` needs none.
const BRACKETED_NAMES: &[&str] = &[
    "__contextual_datatable",
    "__crossCluster",
    "__crossDB",
    "__executeAndCache",
    "__id",
    "__isFuzzy",
    "__noWithSource",
    "__packedColumn",
    "__projectAway",
    "__sourceColumnIndex",
    "accumulate",
    "and",
    "anomalychart",
    "areachart",
    "as",
    "asc",
    "bagexpansion",
    "barchart",
    "between",
    "bin_legacy",
    "boolean",
    "by",
    "byte",
    "cachingpolicy",
    "callout",
    "cancel",
    "card",
    "char",
    "columnchart",
    "contains",
    "contains_cs",
    "containscs",
    "cycles",
    "dataexport",
    "datascope",
    "datatable",
    "date",
    "datetime",
    "decimal",
    "decodeblocks",
    "desc",
    "double",
    "dynamic",
    "earliest",
    "encodingpolicy",
    "endswith",
    "endswith_cs",
    "expandoutput",
    "extent_tags_retention",
    "external_data",
    "externaldata",
    "find",
    "first",
    "flags",
    "float",
    "force_remote",
    "harddelete",
    "hardretention",
    "has",
    "has_all",
    "has_any",
    "has_cs",
    "hasprefix",
    "hasprefix_cs",
    "hassuffix",
    "hassuffix_cs",
    "hotcache",
    "in",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "invoke",
    "isfuzzy",
    "journal",
    "kind",
    "ladderchart",
    "last",
    "latest",
    "like",
    "likecs",
    "linechart",
    "long",
    "materialize",
    "mdm",
    "missing",
    "nooptimization",
    "notcontains",
    "notcontainscs",
    "notlike",
    "notlikecs",
    "of",
    "or",
    "others",
    "pathformat",
    "piechart",
    "pivotchart",
    "print",
    "project",
    "queries",
    "query_results",
    "real",
    "relaxed",
    "restricted_view_access",
    "row_level_security",
    "rowstore",
    "rowstore_references",
    "rowstore_sealinfo",
    "rowstorepolicy",
    "rowstores",
    "sample",
    "scatterchart",
    "seal",
    "seals",
    "search",
    "set",
    "shards",
    "simple",
    "softdelete",
    "softretention",
    "sql",
    "stackedareachart",
    "startswith",
    "startswith_cs",
    "statistics",
    "storedqueryresultcontainers",
    "string",
    "tablepurge",
    "time",
    "timechart",
    "timeline",
    "timepivot",
    "timespan",
    "title",
    "to",
    "toscalar",
    "totable",
    "treemap",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "ulong",
    "union",
    "uniqueid",
    "unrestrictedviewers",
    "verbose",
    "viewers",
    "views",
    "where",
    "with_itemindex",
    "with_match_id",
    "with_source",
    "with_step_name",
    "withsource",
    "writeaheadlog",
];

/// A KQL string literal: single-quoted, or double-quoted when the text holds a single quote.
/// Backslashes and control characters are escaped; other characters are kept as they are.
fn string_literal(text: &str) -> String {
    let quote = if text.contains('\'') { '"' } else { '\'' };
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push(quote);
    for character in text.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '\u{7}' => literal.push_str("\\a"),
            '\u{8}' => literal.push_str("\\b"),
            '\t' => literal.push_str("\\t"),
            '\n' => literal.push_str("\\n"),
            '\u{c}' => literal.push_str("\\f"),
            '\r' => literal.push_str("\\r"),
            other if other == quote => {
                literal.push('\\');
                literal.push(other);
            }
            other => literal.push(other),
        }
    }
    literal.push(quote);
    literal
}

fn column_reference(name: &str) -> String {
    let is_identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    if is_identifier && !BRACKETED_NAMES.contains(&name) {
        name.to_string()
    } else {
        format!("[{}]", string_literal(name))
    }
}

fn kql_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Bool => "bool",
        ColumnKind::Int => "int",
        ColumnKind::Long => "long",
        ColumnKind::Real => "real",
        ColumnKind::Decimal => "decimal",
        ColumnKind::DateTime => "datetime",
        ColumnKind::TimeSpan => "timespan",
        ColumnKind::Guid => "guid",
        ColumnKind::Dynamic => "dynamic",
        ColumnKind::String | ColumnKind::Other => "string",
    }
}

/// A real written with a decimal point, and never in exponent form, so it stays a real.
fn real_literal(cell: &Cell) -> String {
    let source = cell.display_text();
    let text = if source.contains(['e', 'E']) {
        // Display for a float never uses an exponent and gives the shortest digits that read
        // back as the same value.
        source
            .parse::<f64>()
            .map_or_else(|_| source.to_string(), |value| value.to_string())
    } else {
        source.into_owned()
    };
    if text.contains('.') {
        text
    } else {
        format!("{text}.0")
    }
}

fn dynamic_literal(cell: &Cell) -> String {
    let json = match cell {
        Cell::Text(text) => serde_json::from_str::<serde_json::Value>(text)
            .unwrap_or_else(|_| serde_json::Value::String(text.clone())),
        other => serde_json::from_str(&other.display_text())
            .unwrap_or_else(|_| serde_json::Value::String(other.display_text().into_owned())),
    };
    format!("dynamic({json})")
}

/// One value as a KQL literal of its column's type. Null is written with its type, except in
/// a string column, where it is an empty string.
fn literal(kind: ColumnKind, cell: Option<&Cell>) -> String {
    let Some(cell) = cell.filter(|cell| !cell.is_null()) else {
        return match kind {
            ColumnKind::String | ColumnKind::Other => "''".to_string(),
            other => format!("{}(null)", kql_type(other)),
        };
    };
    match kind {
        ColumnKind::Bool => cell.display_text().to_ascii_lowercase(),
        ColumnKind::Long => cell.display_text().into_owned(),
        ColumnKind::Real => real_literal(cell),
        ColumnKind::Int | ColumnKind::Decimal | ColumnKind::Guid => {
            format!("{}({})", kql_type(kind), cell.display_text())
        }
        ColumnKind::DateTime | ColumnKind::TimeSpan => {
            format!("{}({})", kql_type(kind), cell.display_text())
        }
        ColumnKind::Dynamic => dynamic_literal(cell),
        ColumnKind::String | ColumnKind::Other => string_literal(&cell.display_text()),
    }
}

/// A `datatable` expression that reproduces the given cells, with each column's type.
///
/// Dates, timespans and guids keep the text the server sent. Differences from the VS Code
/// extension, which takes its literals from .NET values: booleans are lower case, reals keep
/// their source digits, and a string inside a dynamic column is quoted as JSON.
pub fn datatable(table: &Table, rows: &[usize], columns: &[usize]) -> String {
    let columns: Vec<usize> = columns
        .iter()
        .copied()
        .filter(|column| *column < table.columns.len())
        .collect();
    let schema = columns
        .iter()
        .map(|column| {
            let column = &table.columns[*column];
            format!(
                "{}: {}",
                column_reference(&column.name),
                kql_type(column.kind)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let body = rows
        .iter()
        .map(|row| {
            let values = columns
                .iter()
                .map(|column| literal(table.columns[*column].kind, table.cell(*row, *column)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("    {values}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    if body.is_empty() {
        format!("datatable ({schema}) [\n]")
    } else {
        format!("datatable ({schema}) [\n{body}\n]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Cell, Column};

    fn sample() -> Table {
        Table {
            name: "t".into(),
            columns: vec![Column::new("Name", "string"), Column::new("N", "long")],
            rows: vec![
                vec![Cell::Text("has\ttab".into()), Cell::Int(1)],
                vec![Cell::Text("say \"x\"".into()), Cell::Null],
                vec![
                    Cell::Text("a|b\nc".into()),
                    Cell::Dynamic(Box::new(serde_json::json!({"k": [1]}))),
                ],
            ],
        }
    }

    #[test]
    fn a_single_cell_copies_as_its_raw_value() {
        assert_eq!(copy_text(&sample(), &[0], &[0]), "has\ttab");
        assert_eq!(copy_text(&sample(), &[1], &[1]), "");
    }

    #[test]
    fn larger_selections_copy_as_quoted_tab_separated_text() {
        assert_eq!(
            copy_text(&sample(), &[0, 1, 2], &[0, 1]),
            "Name\tN\n\"has\ttab\"\t1\n\"say \"\"x\"\"\"\t\n\"a|b\nc\"\t\"{\"\"k\"\":[1]}\""
        );
    }

    #[test]
    fn writes_columns_and_rows_in_the_order_given() {
        assert_eq!(
            tsv(&sample(), &[1, 0], &[1, 0]),
            "N\tName\n\t\"say \"\"x\"\"\"\n1\t\"has\ttab\""
        );
        assert_eq!(tsv(&sample(), &[0], &[]), "");
    }

    #[test]
    fn markdown_escapes_pipes_and_flattens_line_breaks() {
        assert_eq!(
            markdown(&sample(), &[2], &[0, 1]),
            "| Name | N |\n| --- | --- |\n| a\\|b c | {\"k\":[1]} |"
        );
    }

    #[test]
    fn html_escapes_markup_and_keeps_the_header() {
        let table = Table {
            name: "t".into(),
            columns: vec![Column::new("A<B", "string")],
            rows: vec![vec![Cell::Text("George <gw@x.com> & \"co\"".into())]],
        };
        let output = html(&table, &[0], &[0]);
        assert!(output.starts_with("<table border=\"1\""));
        assert!(output.contains(">A&lt;B</th>"));
        assert!(output.contains(">George &lt;gw@x.com&gt; &amp; &quot;co&quot;</td>"));
        assert!(output.ends_with("</tbody></table>"));
    }

    fn every_type() -> Table {
        let names_and_types = [
            ("Id", "long"),
            ("Time Stamp", "datetime"),
            ("Elapsed", "timespan"),
            ("Flag", "bool"),
            ("Score", "real"),
            ("Count", "int"),
            ("Guid", "guid"),
            ("Message", "string"),
            ("where", "string"),
            ("Payload", "dynamic"),
        ];
        Table {
            name: "t".into(),
            columns: names_and_types
                .iter()
                .map(|(name, type_name)| Column::new(*name, *type_name))
                .collect(),
            rows: vec![
                vec![
                    Cell::Int(1),
                    Cell::Text("2026-09-30T14:42:11.4020000Z".into()),
                    Cell::Text("00:00:30.5000000".into()),
                    Cell::Bool(true),
                    Cell::Real(3.25),
                    Cell::Int(7),
                    Cell::Text("8a1c2f4e-0000-4000-8000-000000000001".into()),
                    Cell::Text("it's \"fine\"".into()),
                    Cell::Text("C:\\dir".into()),
                    Cell::Dynamic(Box::new(serde_json::json!({"a": [1, 2], "b": null}))),
                ],
                vec![Cell::Null; 10],
                vec![
                    Cell::Int(i64::MIN),
                    Cell::Text("2026-01-01T05:00:00.0000000Z".into()),
                    Cell::Text("-1.02:03:04.5000000".into()),
                    Cell::Bool(false),
                    Cell::Real(0.1),
                    Cell::Int(-5),
                    Cell::Text("00000000-0000-0000-0000-000000000000".into()),
                    Cell::Text("line1\nline2\ttab".into()),
                    Cell::Text("x".into()),
                    Cell::Dynamic(Box::new(serde_json::json!([]))),
                ],
            ],
        }
    }

    /// The expected text is what the VS Code extension's `KustoGenerator` writes for the same
    /// table (Kusto.Language 12.4.0), apart from the three deliberate differences: `true` and
    /// `false` in lower case, and exact reals.
    #[test]
    fn datatable_matches_the_vs_code_generator() {
        let table = every_type();
        let all_rows: Vec<usize> = (0..table.rows.len()).collect();
        let all_columns: Vec<usize> = (0..table.columns.len()).collect();
        assert_eq!(
            datatable(&table, &all_rows, &all_columns),
            "datatable (Id: long, ['Time Stamp']: datetime, Elapsed: timespan, Flag: bool, Score: real, Count: int, Guid: guid, Message: string, ['where']: string, Payload: dynamic) [\n    \
1, datetime(2026-09-30T14:42:11.4020000Z), timespan(00:00:30.5000000), true, 3.25, int(7), guid(8a1c2f4e-0000-4000-8000-000000000001), \"it's \\\"fine\\\"\", 'C:\\\\dir', dynamic({\"a\":[1,2],\"b\":null}),\n    \
long(null), datetime(null), timespan(null), bool(null), real(null), int(null), guid(null), '', '', dynamic(null),\n    \
-9223372036854775808, datetime(2026-01-01T05:00:00.0000000Z), timespan(-1.02:03:04.5000000), false, 0.1, int(-5), guid(00000000-0000-0000-0000-000000000000), 'line1\\nline2\\ttab', 'x', dynamic([])\n]"
        );
    }

    #[test]
    fn datatable_writes_the_selected_cells_in_the_order_given() {
        assert_eq!(
            datatable(&every_type(), &[2, 0], &[7, 0]),
            "datatable (Message: string, Id: long) [\n    'line1\\nline2\\ttab', -9223372036854775808,\n    \"it's \\\"fine\\\"\", 1\n]"
        );
        assert_eq!(
            datatable(&every_type(), &[], &[0]),
            "datatable (Id: long) [\n]"
        );
    }

    /// Each case below was checked against `KustoFacts.GetStringLiteral`.
    #[test]
    fn string_literals_choose_their_quote_and_escape_like_kusto() {
        for (text, expected) in [
            ("plain", "'plain'"),
            ("it's", "\"it's\""),
            ("say \"hi\"", "'say \"hi\"'"),
            ("both ' and \"", "\"both ' and \\\"\""),
            ("back\\slash", "'back\\\\slash'"),
            ("it's \\ back", "\"it's \\\\ back\""),
            ("line1\nline2", "'line1\\nline2'"),
            ("a\r\nb", "'a\\r\\nb'"),
            ("tab\there", "'tab\\there'"),
            ("bell\u{7}", "'bell\\a'"),
            ("back\u{8}space", "'back\\bspace'"),
            ("form\u{c}feed", "'form\\ffeed'"),
            ("vertical\u{b}tab", "'vertical\u{b}tab'"),
            ("nul\u{0}x", "'nul\u{0}x'"),
            ("unicode é ☃ 😀", "'unicode é ☃ 😀'"),
            ("", "''"),
        ] {
            assert_eq!(string_literal(text), expected, "{text:?}");
        }
    }

    /// Checked against `KustoFacts.BracketNameIfNecessary`: anything that is not an ASCII
    /// identifier, and Kusto's keywords, go in brackets. Keywords are case-sensitive.
    #[test]
    fn column_names_are_bracketed_when_kusto_needs_it() {
        for (name, expected) in [
            ("Id", "Id"),
            ("_under", "_under"),
            ("x1", "x1"),
            ("Count", "Count"),
            ("let", "let"),
            ("type", "type"),
            ("table", "table"),
            ("Where", "Where"),
            ("DATETIME", "DATETIME"),
            ("Time Stamp", "['Time Stamp']"),
            ("1abc", "['1abc']"),
            ("with-dash", "['with-dash']"),
            ("a.b", "['a.b']"),
            ("ünï", "['ünï']"),
            ("名前", "['名前']"),
            ("", "['']"),
            ("where", "['where']"),
            ("datetime", "['datetime']"),
            ("string", "['string']"),
            ("by", "['by']"),
            ("project", "['project']"),
            ("it's", "[\"it's\"]"),
            ("a'b\"c", "[\"a'b\\\"c\"]"),
            ("new\nline", "['new\\nline']"),
        ] {
            assert_eq!(column_reference(name), expected, "{name:?}");
        }
    }

    #[test]
    fn reals_keep_a_decimal_point_and_their_digits() {
        let real = |cell: Cell| literal(ColumnKind::Real, Some(&cell));
        assert_eq!(real(Cell::Real(3.25)), "3.25");
        assert_eq!(real(Cell::Real(5.0)), "5.0");
        assert_eq!(real(Cell::Int(5)), "5.0");
        assert_eq!(real(Cell::Real(0.1 + 0.2)), "0.30000000000000004");
        assert_eq!(real(Cell::Real(-0.5)), "-0.5");
        assert_eq!(real(Cell::Real(1.5e-7)), "0.00000015");
    }

    #[test]
    fn dynamic_columns_hold_json_even_for_text_and_scalars() {
        let dynamic = |cell: Cell| literal(ColumnKind::Dynamic, Some(&cell));
        assert_eq!(
            dynamic(Cell::Text("{\"a\": 1}".into())),
            "dynamic({\"a\":1})"
        );
        assert_eq!(dynamic(Cell::Text("abc".into())), "dynamic(\"abc\")");
        assert_eq!(dynamic(Cell::Int(5)), "dynamic(5)");
        assert_eq!(dynamic(Cell::Bool(true)), "dynamic(true)");
    }
}
