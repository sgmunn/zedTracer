//! What Share puts on the clipboard: a link that opens the query in Azure Data Explorer, the query
//! itself and the first rows of its result, for whichever of them the person chose.

use anyhow::{Context as _, Result};
use async_compression::futures::bufread::GzipEncoder;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures::AsyncReadExt as _;
use kusto_results::Table;
use kusto_results::export::tsv;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::Cluster;

/// How many rows of a result the preview holds.
pub const PREVIEW_ROWS: usize = 10;

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

/// The first rows of a table as tab-separated text with a header, and a last line that says how
/// many of the rows these are when there are more. A table with no columns has no preview.
pub fn results_preview(table: &Table) -> Option<String> {
    if table.columns.is_empty() {
        return None;
    }
    let columns: Vec<usize> = (0..table.columns.len()).collect();
    let shown = table.rows.len().min(PREVIEW_ROWS);
    let rows: Vec<usize> = (0..shown).collect();
    let mut text = tsv(table, &rows, &columns);
    if table.rows.len() > shown {
        text.push_str(&format!("\n({shown} of {} rows)", table.rows.len()));
    }
    Some(text)
}

/// The parts of what is shared. A part that is `None` is left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shared<'a> {
    pub link: Option<&'a str>,
    pub query: Option<&'a str>,
    pub results_preview: Option<&'a str>,
}

/// The text for the clipboard: the parts that are there, in the order the web explorer gives
/// them, with a blank line between.
pub fn share_text(shared: Shared) -> String {
    [shared.link, shared.query, shared.results_preview]
        .into_iter()
        .flatten()
        .map(str::trim_end)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
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

    #[test]
    fn the_preview_is_the_header_and_the_rows() {
        assert_eq!(
            results_preview(&table(2)).as_deref(),
            Some("n\tlabel\n0\trow 0\n1\trow 1")
        );
    }

    #[test]
    fn the_preview_stops_at_ten_rows_and_says_how_many_there_are() {
        let preview = results_preview(&table(25)).expect("a preview");
        let lines: Vec<_> = preview.lines().collect();
        assert_eq!(lines.len(), 1 + PREVIEW_ROWS + 1);
        assert_eq!(lines[PREVIEW_ROWS], "9\trow 9");
        assert_eq!(lines[PREVIEW_ROWS + 1], "(10 of 25 rows)");
    }

    #[test]
    fn a_table_of_exactly_ten_rows_has_no_note() {
        let preview = results_preview(&table(PREVIEW_ROWS)).expect("a preview");
        assert!(!preview.contains("rows)"), "{preview}");
    }

    #[test]
    fn a_table_with_no_rows_still_shows_its_columns() {
        assert_eq!(results_preview(&table(0)).as_deref(), Some("n\tlabel"));
    }

    #[test]
    fn a_table_with_no_columns_has_no_preview() {
        let table = Table {
            name: "PrimaryResult".into(),
            columns: Vec::new(),
            rows: Vec::new(),
        };
        assert_eq!(results_preview(&table), None);
    }

    #[test]
    fn the_parts_are_in_the_order_of_the_web_explorer_with_a_blank_line_between() {
        assert_eq!(
            share_text(Shared {
                link: Some("https://link"),
                query: Some("T\n| take 1\n"),
                results_preview: Some("n\n1"),
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
                results_preview: Some("n\n1"),
            }),
            "T\n\nn\n1"
        );
        assert_eq!(share_text(Shared::default()), "");
    }
}
