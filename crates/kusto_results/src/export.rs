//! Text forms of a table selection for the clipboard: plain text (tab-separated), Markdown
//! and HTML.
//!
//! Every function takes the source rows and columns to write, in the order to write them, so
//! the caller decides between a selection, the current view or the whole table. Values are
//! written as the source data, not as the inspector displays them.

use crate::result::Table;

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
}
