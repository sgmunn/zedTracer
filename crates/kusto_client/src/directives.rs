//! Where a query runs, as the file says.
//!
//! A line that is a comment of the form `//:setDefaultCluster("https://…")` or
//! `//:setDefaultDb("…")` sets the cluster or the database for every query below it, until a later
//! line sets it again. Setting the cluster clears the database, because a database name from
//! another cluster is more likely to fail confusingly than to be the right one. Before the
//! first directive the defaults apply, which come from the settings.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::query_text::{query_blocks, query_range_at};

static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*//:\s*([A-Za-z]+)\s*\(\s*(?:"([^"]*)"|'([^']*)')\s*\)\s*$"#)
        .expect("the directive pattern is valid")
});

/// The cluster and database a query runs on, as written: not yet checked or normalized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connection {
    pub cluster: Option<String>,
    pub database: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedQuery {
    pub range: Range<usize>,
    pub connection: Connection,
}

enum Directive {
    SetCluster(String),
    SetDatabase(String),
}

fn directive_in(line: &str) -> Option<Directive> {
    let captures = DIRECTIVE.captures(line)?;
    let value = captures
        .get(2)
        .or_else(|| captures.get(3))
        .map(|value| value.as_str().to_string())?;
    match &captures[1] {
        "setDefaultCluster" => Some(Directive::SetCluster(value)),
        "setDefaultDb" => Some(Directive::SetDatabase(value)),
        _ => None,
    }
}

/// The connection after every directive on a line that starts at or before `offset`.
pub fn connection_up_to(text: &str, offset: usize, defaults: &Connection) -> Connection {
    let mut connection = defaults.clone();
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        if line_start > offset {
            break;
        }
        match directive_in(line) {
            Some(Directive::SetCluster(cluster)) => {
                connection = Connection {
                    cluster: Some(cluster),
                    database: None,
                }
            }
            Some(Directive::SetDatabase(database)) => connection.database = Some(database),
            None => {}
        }
        line_start += line.len();
    }
    connection
}

/// The connection of each query of the text, in order.
pub fn connections_of_queries(text: &str, defaults: &Connection) -> Vec<Connection> {
    query_blocks(text)
        .into_iter()
        .map(|range| connection_up_to(text, range.end, defaults))
        .collect()
}

/// The query around the cursor and where it runs.
pub fn resolve_query_at(text: &str, offset: usize, defaults: &Connection) -> Option<ResolvedQuery> {
    let range = query_range_at(text, offset)?;
    let connection = connection_up_to(text, range.end, defaults);
    Some(ResolvedQuery { range, connection })
}

/// Where selected text runs: where the query it starts in does, or else the query below it.
pub fn connection_for_selection(
    text: &str,
    selection: Range<usize>,
    defaults: &Connection,
) -> Connection {
    let limit = query_blocks(text)
        .into_iter()
        .find(|range| range.end >= selection.start)
        .map_or(selection.start, |range| range.end);
    connection_up_to(text, limit, defaults)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        text: String,
        defaults: Option<Expected>,
        queries: Vec<Expected>,
    }

    #[derive(Deserialize, Default)]
    struct Expected {
        cluster: Option<String>,
        database: Option<String>,
    }

    impl From<Expected> for Connection {
        fn from(expected: Expected) -> Self {
            Connection {
                cluster: expected.cluster,
                database: expected.database,
            }
        }
    }

    /// The same cases the language server is tested with, so the two cannot disagree.
    #[test]
    fn the_shared_cases_resolve_as_expected() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fork-docs/samples/connection-directives.json"
        );
        let cases: Vec<Case> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the cases file reads"))
                .expect("the cases file parses");
        assert!(!cases.is_empty());
        for case in cases {
            let defaults: Connection = case.defaults.unwrap_or_default().into();
            let expected: Vec<Connection> = case.queries.into_iter().map(Into::into).collect();
            assert_eq!(
                connections_of_queries(&case.text, &defaults),
                expected,
                "{}",
                case.name
            );
        }
    }

    fn defaults() -> Connection {
        Connection {
            cluster: Some("https://a.kusto.windows.net".into()),
            database: Some("da".into()),
        }
    }

    #[test]
    fn the_cursor_picks_the_query_and_its_connection() {
        let text = "//:setDefaultDb(\"one\")\nT1\n\n//:setDefaultDb(\"two\")\nT2";
        let first = resolve_query_at(text, text.find("T1").unwrap(), &defaults()).unwrap();
        assert_eq!(&text[first.range], "//:setDefaultDb(\"one\")\nT1");
        assert_eq!(first.connection.database.as_deref(), Some("one"));

        let second = resolve_query_at(text, text.len(), &defaults()).unwrap();
        assert_eq!(&text[second.range], "//:setDefaultDb(\"two\")\nT2");
        assert_eq!(second.connection.database.as_deref(), Some("two"));
    }

    #[test]
    fn a_cursor_on_a_directive_between_queries_runs_the_query_above_with_its_own_connection() {
        let text = "T1\n\n//:setDefaultDb(\"two\")\n\nT2";
        let resolved = resolve_query_at(text, text.find("two").unwrap(), &defaults()).unwrap();
        assert_eq!(&text[resolved.range], "T1");
        assert_eq!(resolved.connection.database.as_deref(), Some("da"));
    }

    #[test]
    fn selected_text_runs_where_its_query_runs() {
        let text = "T1\n\n//:setDefaultDb(\"two\")\n\nT2 | take 1";
        let selection = text.find("T2").unwrap()..text.len();
        assert_eq!(
            connection_for_selection(text, selection, &defaults()).database.as_deref(),
            Some("two")
        );
        let first_line = 0..2;
        assert_eq!(
            connection_for_selection(text, first_line, &defaults()).database.as_deref(),
            Some("da")
        );
    }
}
