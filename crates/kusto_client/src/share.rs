//! What Share puts on the clipboard: a link that opens the query in Azure Data Explorer, the query
//! itself and the rows of its result, for whichever of them the person chose.

use anyhow::{Context as _, Result};
use async_compression::futures::bufread::GzipEncoder;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures::AsyncReadExt as _;
use kusto_results::Table;
use kusto_results::export::{escape_html, html as html_table, markdown, tsv};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::Cluster;

const WEB_EXPLORER: &str = "https://dataexplorer.azure.com";

/// The domains the web explorer leaves out of the name of a cluster in its own links, as it does
/// for `help` and for a Fabric eventhouse.
const OMITTED_DOMAINS: [&str; 2] = [".kusto.windows.net", ".kusto.fabric.microsoft.com"];

/// What the web explorer leaves unescaped in a link: letters, digits and `-._~`. A database named
/// by a GUID keeps its dashes, as it does in the links the explorer writes.
const ESCAPED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A link that opens `query` in the web explorer on `cluster` and `database`. The query travels in
/// the link as `base64(gzip(query))`, which is how the explorer reads a long query out of its
/// address.
pub async fn web_explorer_link(cluster: &str, database: &str, query: &str) -> Result<String> {
    let cluster = Cluster::parse(cluster)?;
    let database = database.trim();
    anyhow::ensure!(!database.is_empty(), "The link needs a database.");
    let name = OMITTED_DOMAINS
        .iter()
        .find_map(|domain| cluster.host().strip_suffix(domain))
        .unwrap_or(cluster.host());
    let mut compressed = Vec::new();
    GzipEncoder::new(futures::io::Cursor::new(query.as_bytes()))
        .read_to_end(&mut compressed)
        .await
        .context("could not compress the query for the link")?;
    Ok(format!(
        "{WEB_EXPLORER}/clusters/{name}/databases/{}?query={}",
        utf8_percent_encode(database, ESCAPED),
        utf8_percent_encode(&STANDARD.encode(compressed), ESCAPED),
    ))
}

/// How the rows of a result are written. Rich text is an HTML table for the programs that take
/// one, with tab-separated text for the rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResultsFormat {
    #[default]
    Markdown,
    RichText,
}

/// The rows of a result, written to be shared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultsPreview {
    /// For the plain-text clipboard: a Markdown table, or for rich text, tab-separated rows.
    pub text: String,
    /// For the rich-text clipboard, and only for rich text.
    pub html: Option<String>,
    /// The last line of `text`, and the last paragraph of `html`, that says how many of the rows
    /// there are, when the rows were cut.
    pub note: Option<String>,
}

impl ResultsPreview {
    /// The text without the note, for a place that says how many rows there are in its own way.
    pub fn text_without_note(&self) -> &str {
        match &self.note {
            Some(note) => self
                .text
                .strip_suffix(note.as_str())
                .unwrap_or(&self.text)
                .trim_end(),
            None => &self.text,
        }
    }
}

/// The first `limit` rows of a table, or all of them with no limit, with a header and a last line
/// that says how many of the rows these are when there are more. A table with no columns has
/// nothing to show.
pub fn results_preview(
    table: &Table,
    limit: Option<usize>,
    format: ResultsFormat,
) -> Option<ResultsPreview> {
    if table.columns.is_empty() {
        return None;
    }
    let columns: Vec<usize> = (0..table.columns.len()).collect();
    let total = table.rows.len();
    let shown = limit.map_or(total, |limit| limit.min(total));
    let rows: Vec<usize> = (0..shown).collect();
    let note = (shown < total).then(|| format!("({shown} of {total} rows)"));
    Some(match format {
        ResultsFormat::Markdown => {
            let mut text = markdown(table, &rows, &columns);
            if let Some(note) = &note {
                text.push_str("\n\n");
                text.push_str(note);
            }
            ResultsPreview {
                text,
                html: None,
                note,
            }
        }
        ResultsFormat::RichText => {
            let mut text = tsv(table, &rows, &columns);
            let mut html = html_table(table, &rows, &columns);
            if let Some(note) = &note {
                text.push('\n');
                text.push_str(note);
                html.push_str(&format!("<p>{note}</p>"));
            }
            ResultsPreview {
                text,
                html: Some(html),
                note,
            }
        }
    })
}

/// The parts of what is shared. A part that is `None` is left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shared<'a> {
    pub link: Option<&'a str>,
    pub query: Option<&'a str>,
    pub results: Option<&'a ResultsPreview>,
}

/// The text for the clipboard: the parts that are there, in the order the web explorer gives
/// them, with a blank line between.
pub fn share_text(shared: Shared) -> String {
    [
        shared.link,
        shared.query,
        shared.results.map(|results| results.text.as_str()),
    ]
    .into_iter()
    .flatten()
    .map(str::trim_end)
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// The rich text for the clipboard, when the rows are rich text. Without a table there is nothing
/// that plain text does not say as well.
pub fn share_html(shared: Shared) -> Option<String> {
    let table = shared.results?.html.as_deref()?;
    let mut html = String::new();
    if let Some(link) = shared.link {
        let link = escape_html(link);
        html.push_str(&format!("<p><a href=\"{link}\">{link}</a></p>"));
    }
    if let Some(query) = shared.query {
        html.push_str(&format!(
            "<pre style=\"font-family: Consolas, Menlo, monospace\">{}</pre>",
            escape_html(query.trim_end())
        ));
    }
    html.push_str(table);
    Some(html)
}

#[cfg(test)]
mod tests {
    use async_compression::futures::bufread::GzipDecoder;
    use futures::executor::block_on;
    use futures::io::BufReader;
    use kusto_results::{Cell, Column};
    use pretty_assertions::assert_eq;

    use super::*;

    /// The query a link carries, read the way the web explorer reads it.
    fn query_of(link: &str) -> String {
        let (_, encoded) = link.split_once("?query=").expect("a link with a query");
        let encoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .expect("UTF-8");
        let compressed = STANDARD.decode(encoded.as_bytes()).expect("base64");
        let mut query = String::new();
        block_on(
            GzipDecoder::new(BufReader::new(compressed.as_slice())).read_to_string(&mut query),
        )
        .expect("gzip");
        query
    }

    fn table(rows: usize) -> Table {
        Table {
            name: "PrimaryResult".into(),
            columns: vec![Column::new("n", "long"), Column::new("label", "string")],
            rows: (0..rows)
                .map(|row| vec![Cell::Int(row as i64), Cell::Text(format!("row {row}"))])
                .collect(),
        }
    }

    #[test]
    fn the_link_names_the_cluster_and_database_and_carries_the_query() {
        let query = "StormEvents\n| where State == 'TEXAS'\n| take 10";
        let link = block_on(web_explorer_link(
            "https://help.kusto.windows.net",
            "Samples",
            query,
        ))
        .expect("a link");
        assert!(
            link.starts_with(
                "https://dataexplorer.azure.com/clusters/help/databases/Samples?query=H4sI"
            ),
            "{link}"
        );
        assert_eq!(query_of(&link), query);
    }

    #[test]
    fn the_link_escapes_what_a_link_cannot_hold() {
        let link = block_on(web_explorer_link("help", "My Samples", "print 1")).expect("a link");
        assert!(link.contains("/databases/My%20Samples?query="), "{link}");
        let (_, query) = link.split_once("?query=").expect("a query");
        assert!(
            !query.contains(['+', '/', '=', ' ']),
            "the base64 is escaped: {query}"
        );
    }

    #[test]
    fn a_database_named_by_a_guid_keeps_its_dashes() {
        let link = block_on(web_explorer_link(
            "help",
            "89574d37-5769-4203-b5d7-683362668f1d",
            "print 1",
        ))
        .expect("a link");
        assert!(
            link.contains("/databases/89574d37-5769-4203-b5d7-683362668f1d?query="),
            "{link}"
        );
    }

    #[test]
    fn a_cluster_keeps_its_region_and_loses_the_domain_the_explorer_assumes() {
        let link = |cluster: &str| {
            let link = block_on(web_explorer_link(cluster, "Db", "print 1")).expect("a link");
            link.split("/databases/")
                .next()
                .expect("a cluster part")
                .trim_start_matches("https://dataexplorer.azure.com/clusters/")
                .to_string()
        };
        assert_eq!(
            link("https://mycluster.westus.kusto.windows.net"),
            "mycluster.westus"
        );
        assert_eq!(link("mycluster.westus"), "mycluster.westus");
        assert_eq!(
            link("https://trd-abc.z9.kusto.fabric.microsoft.com"),
            "trd-abc.z9"
        );
        assert_eq!(
            link("https://mycluster.kusto.chinacloudapi.cn"),
            "mycluster.kusto.chinacloudapi.cn"
        );
    }

    #[test]
    fn a_query_with_non_ascii_text_survives_the_link() {
        let query = "print 'naïve — 数据'";
        let link = block_on(web_explorer_link("help", "Samples", query)).expect("a link");
        assert_eq!(query_of(&link), query);
    }

    #[test]
    fn a_link_with_no_database_or_a_bad_cluster_is_an_error() {
        assert!(block_on(web_explorer_link("help", "  ", "print 1")).is_err());
        assert!(block_on(web_explorer_link("http://help", "Samples", "print 1")).is_err());
    }

    fn preview(rows: usize, limit: Option<usize>, format: ResultsFormat) -> ResultsPreview {
        results_preview(&table(rows), limit, format).expect("a preview")
    }

    #[test]
    fn markdown_rows_are_a_table() {
        assert_eq!(
            preview(2, None, ResultsFormat::Markdown),
            ResultsPreview {
                text: "| n | label |\n| --- | --- |\n| 0 | row 0 |\n| 1 | row 1 |".into(),
                html: None,
                note: None,
            }
        );
    }

    #[test]
    fn rich_text_rows_are_an_html_table_over_tab_separated_text() {
        let preview = preview(2, None, ResultsFormat::RichText);
        assert_eq!(preview.text, "n\tlabel\n0\trow 0\n1\trow 1");
        let html = preview.html.expect("HTML");
        assert!(html.starts_with("<table "), "{html}");
        assert_eq!(html.matches("<tr>").count(), 3, "{html}");
        assert!(html.contains(">row 1</td>"), "{html}");
    }

    #[test]
    fn with_no_limit_every_row_is_there_and_nothing_is_said_about_a_count() {
        let preview = preview(25, None, ResultsFormat::Markdown);
        assert_eq!(preview.text.lines().count(), 2 + 25);
        assert!(!preview.text.contains("rows)"), "{}", preview.text);
    }

    #[test]
    fn a_limit_keeps_the_first_rows_and_says_how_many_there_are() {
        let markdown = preview(25, Some(3), ResultsFormat::Markdown);
        let lines: Vec<_> = markdown.text.lines().collect();
        assert_eq!(lines.len(), 2 + 3 + 2);
        assert_eq!(lines[4], "| 2 | row 2 |");
        assert_eq!(lines[6], "(3 of 25 rows)");

        let rich = preview(25, Some(3), ResultsFormat::RichText);
        assert_eq!(rich.text.lines().last(), Some("(3 of 25 rows)"));
        let html = rich.html.expect("HTML");
        assert_eq!(html.matches("<tr>").count(), 1 + 3);
        assert!(html.ends_with("<p>(3 of 25 rows)</p>"), "{html}");
    }

    #[test]
    fn the_note_can_be_taken_off_the_text() {
        let cut = preview(25, Some(3), ResultsFormat::Markdown);
        assert_eq!(cut.text_without_note().lines().count(), 2 + 3);
        let rich = preview(25, Some(3), ResultsFormat::RichText);
        assert_eq!(
            rich.text_without_note(),
            "n\tlabel\n0\trow 0\n1\trow 1\n2\trow 2"
        );
        let whole = preview(2, None, ResultsFormat::Markdown);
        assert_eq!(whole.text_without_note(), whole.text);
    }

    #[test]
    fn a_limit_that_is_as_big_as_the_table_or_bigger_leaves_no_note() {
        for limit in [25, 26, 1000] {
            let preview = preview(25, Some(limit), ResultsFormat::Markdown);
            assert!(!preview.text.contains("rows)"), "{}", preview.text);
            assert_eq!(preview.text.lines().count(), 2 + 25);
        }
    }

    #[test]
    fn a_limit_of_nothing_shows_the_header_only() {
        let preview = preview(5, Some(0), ResultsFormat::Markdown);
        assert_eq!(
            preview.text,
            "| n | label |\n| --- | --- |\n\n(0 of 5 rows)"
        );
    }

    #[test]
    fn a_table_with_no_rows_still_shows_its_columns() {
        assert_eq!(
            preview(0, None, ResultsFormat::Markdown).text,
            "| n | label |\n| --- | --- |"
        );
    }

    #[test]
    fn a_table_with_no_columns_has_no_preview() {
        let table = Table {
            name: "PrimaryResult".into(),
            columns: Vec::new(),
            rows: Vec::new(),
        };
        assert_eq!(results_preview(&table, None, ResultsFormat::Markdown), None);
    }

    fn rows_of_text(text: &str) -> ResultsPreview {
        ResultsPreview {
            text: text.into(),
            html: None,
            note: None,
        }
    }

    #[test]
    fn the_parts_are_in_the_order_of_the_web_explorer_with_a_blank_line_between() {
        assert_eq!(
            share_text(Shared {
                link: Some("https://link"),
                query: Some("T\n| take 1\n"),
                results: Some(&rows_of_text("n\n1")),
            }),
            "https://link\n\nT\n| take 1\n\nn\n1"
        );
    }

    #[test]
    fn a_part_that_is_left_out_leaves_no_gap() {
        assert_eq!(
            share_text(Shared {
                link: None,
                query: Some("T"),
                results: Some(&rows_of_text("n\n1")),
            }),
            "T\n\nn\n1"
        );
        assert_eq!(share_text(Shared::default()), "");
    }

    #[test]
    fn rich_text_holds_the_link_the_query_and_the_table() {
        let results = preview(1, None, ResultsFormat::RichText);
        let html = share_html(Shared {
            link: Some("https://link?a=1&b=2"),
            query: Some("T\n| where x < 1\n"),
            results: Some(&results),
        })
        .expect("HTML");
        assert!(
            html.starts_with(
                "<p><a href=\"https://link?a=1&amp;b=2\">https://link?a=1&amp;b=2</a></p><pre "
            ),
            "{html}"
        );
        assert!(html.contains(">T\n| where x &lt; 1</pre><table "), "{html}");
        assert!(html.ends_with("</table>"), "{html}");
    }

    #[test]
    fn rich_text_leaves_out_the_parts_that_were_not_chosen() {
        let results = preview(1, None, ResultsFormat::RichText);
        let html = share_html(Shared {
            link: None,
            query: None,
            results: Some(&results),
        })
        .expect("HTML");
        assert!(html.starts_with("<table "), "{html}");
    }

    #[test]
    fn without_a_rich_text_table_there_is_no_rich_text() {
        let shared = Shared {
            link: Some("https://link"),
            query: Some("T"),
            results: Some(&preview(1, None, ResultsFormat::Markdown)),
        };
        assert_eq!(share_html(shared), None);
        assert_eq!(share_html(Shared::default()), None);
    }
}
