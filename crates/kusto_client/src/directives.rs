//! What the file says about how its queries run.
//!
//! A line that is a comment of the form `// :setDefaultCluster("https://…")` or
//! `// :setDefaultDb("…")` sets the cluster or the database for every query below it, until a later
//! line sets it again. Setting the cluster clears the database, because a database name from
//! another cluster is more likely to fail confusingly than to be the right one. Before the
//! first directive the defaults apply, which come from the settings.
//!
//! `// :setParam("raid", "abc-123")` gives a parameter that queries declare a value for every query
//! below it, until a later `setParam` of the same name gives another, `// :clearParam("raid")` takes
//! it away, or `// :clearParams()` takes away every one. These values are the file's own, so they
//! come before those of the active parameter profile.
//!
//! The space after the slashes is optional: Zed adds `// ` when a comment line is continued, so
//! `// :` is what gets typed, and `//:` is what other Kusto extensions write. An argument is
//! a string in double or single quotes, written as it is: there are no escapes, so a value with
//! one kind of quote in it is written with the other.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::parameters::is_parameter_name;
use crate::query_text::{query_blocks, query_range_at};

const QUOTED: &str = r#"(?:"[^"]*"|'[^']*')"#;

static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*//\s*:\s*([A-Za-z]+)\s*\((.*)\)\s*$").expect("the directive pattern is valid")
});

static ARGUMENT_LIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"^\s*(?:{QUOTED}(?:\s*,\s*{QUOTED})*)?\s*$"))
        .expect("the argument list pattern is valid")
});

static ARGUMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""([^"]*)"|'([^']*)'"#).expect("the argument pattern is valid")
});

/// The file, in Zed's Kusto folder, where the editor tells the language server which cluster and
/// database queries run on when a file does not say.
pub const DEFAULTS_FILE: &str = "defaults.json";

/// The cluster and database a query runs on, as written: not yet checked or normalized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connection {
    pub cluster: Option<String>,
    pub database: Option<String>,
}

impl Connection {
    /// What goes in [DEFAULTS_FILE].
    pub fn to_defaults_file(&self) -> String {
        serde_json::json!({ "cluster": self.cluster, "database": self.database }).to_string()
    }
}

/// What the directives above a point set: where a query runs, and the values the file gives to
/// the parameters a query declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectiveState {
    pub connection: Connection,
    pub parameters: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedQuery {
    pub range: Range<usize>,
    pub connection: Connection,
    pub parameters: BTreeMap<String, String>,
}

enum Directive {
    SetCluster(String),
    SetDatabase(String),
    SetParameter { name: String, value: String },
    ClearParameter(String),
    ClearParameters,
}

fn directive_in(line: &str) -> Option<Directive> {
    let captures = DIRECTIVE.captures(line)?;
    let name = captures.get(1)?.as_str();
    let list = captures.get(2)?.as_str();
    if !ARGUMENT_LIST.is_match(list) {
        return None;
    }
    let arguments: Vec<String> = ARGUMENT
        .captures_iter(list)
        .filter_map(|argument| {
            let text = argument.get(1).or_else(|| argument.get(2))?;
            Some(text.as_str().to_string())
        })
        .collect();
    match (name, arguments.as_slice()) {
        ("setDefaultCluster", [cluster]) => Some(Directive::SetCluster(cluster.clone())),
        ("setDefaultDb", [database]) => Some(Directive::SetDatabase(database.clone())),
        ("setParam", [name, value]) if is_parameter_name(name) => Some(Directive::SetParameter {
            name: name.clone(),
            value: value.clone(),
        }),
        ("clearParam", [name]) if is_parameter_name(name) => {
            Some(Directive::ClearParameter(name.clone()))
        }
        ("clearParams", []) => Some(Directive::ClearParameters),
        _ => None,
    }
}

/// What every directive on a line that starts at or before `offset` has set.
pub fn state_up_to(text: &str, offset: usize, defaults: &Connection) -> DirectiveState {
    let mut state = DirectiveState {
        connection: defaults.clone(),
        parameters: BTreeMap::new(),
    };
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        if line_start > offset {
            break;
        }
        match directive_in(line) {
            Some(Directive::SetCluster(cluster)) => {
                state.connection = Connection {
                    cluster: Some(cluster),
                    database: None,
                }
            }
            Some(Directive::SetDatabase(database)) => state.connection.database = Some(database),
            Some(Directive::SetParameter { name, value }) => {
                state.parameters.insert(name, value);
            }
            Some(Directive::ClearParameter(name)) => {
                state.parameters.remove(&name);
            }
            Some(Directive::ClearParameters) => state.parameters.clear(),
            None => {}
        }
        line_start += line.len();
    }
    state
}

/// The connection after every directive on a line that starts at or before `offset`.
pub fn connection_up_to(text: &str, offset: usize, defaults: &Connection) -> Connection {
    state_up_to(text, offset, defaults).connection
}

/// The connection of each query of the text, in order.
pub fn connections_of_queries(text: &str, defaults: &Connection) -> Vec<Connection> {
    query_blocks(text)
        .into_iter()
        .map(|range| connection_up_to(text, range.end, defaults))
        .collect()
}

/// The query around the cursor, where it runs and the values the file gives its parameters.
pub fn resolve_query_at(text: &str, offset: usize, defaults: &Connection) -> Option<ResolvedQuery> {
    let range = query_range_at(text, offset)?;
    let DirectiveState {
        connection,
        parameters,
    } = state_up_to(text, range.end, defaults);
    Some(ResolvedQuery {
        range,
        connection,
        parameters,
    })
}

/// What applies to selected text: what applies to the query it starts in, or else the query below it.
pub fn state_for_selection(
    text: &str,
    selection: Range<usize>,
    defaults: &Connection,
) -> DirectiveState {
    let limit = query_blocks(text)
        .into_iter()
        .find(|range| range.end >= selection.start)
        .map_or(selection.start, |range| range.end);
    state_up_to(text, limit, defaults)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[test]
    fn the_defaults_file_names_both_settings_even_when_one_is_missing() {
        let connection = Connection {
            cluster: Some("https://help.kusto.windows.net".into()),
            database: None,
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&connection.to_defaults_file()).ok(),
            Some(
                serde_json::json!({ "cluster": "https://help.kusto.windows.net", "database": null })
            )
        );
    }

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

    #[derive(Deserialize)]
    struct ParameterCase {
        name: String,
        text: String,
        queries: Vec<BTreeMap<String, String>>,
    }

    /// The same cases the language server is tested with, through the lens that names the
    /// parameters a file gives to a query.
    #[test]
    fn the_shared_parameter_cases_resolve_as_expected() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fork-docs/samples/parameter-directives.json"
        );
        let cases: Vec<ParameterCase> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the cases file reads"))
                .expect("the cases file parses");
        assert!(!cases.is_empty());
        for case in cases {
            let resolved: Vec<BTreeMap<String, String>> = query_blocks(&case.text)
                .into_iter()
                .map(|range| state_up_to(&case.text, range.end, &defaults()).parameters)
                .collect();
            assert_eq!(resolved, case.queries, "{}", case.name);
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
    fn the_cursor_picks_the_parameters_of_its_query() {
        let text = "//:setParam(\"raid\", \"one\")\nT1\n\n//:setParam(\"raid\", \"two\")\n\nT2";
        let first = resolve_query_at(text, text.find("T1").unwrap(), &defaults()).unwrap();
        assert_eq!(
            first.parameters,
            BTreeMap::from([("raid".to_string(), "one".to_string())])
        );
        let second = resolve_query_at(text, text.len(), &defaults()).unwrap();
        assert_eq!(
            second.parameters,
            BTreeMap::from([("raid".to_string(), "two".to_string())])
        );

        let above = "T1\n\n//:setParam(\"raid\", \"two\")\n\nT2";
        let on_directive = resolve_query_at(above, above.find("two").unwrap(), &defaults()).unwrap();
        assert_eq!(&above[on_directive.range], "T1");
        assert!(
            on_directive.parameters.is_empty(),
            "a cursor on a directive runs the query above, which the directive does not reach"
        );
    }

    #[test]
    fn selected_text_runs_where_its_query_runs() {
        let text = "T1\n\n//:setDefaultDb(\"two\")\n\nT2 | take 1";
        let selection = text.find("T2").unwrap()..text.len();
        assert_eq!(
            state_for_selection(text, selection, &defaults())
                .connection
                .database
                .as_deref(),
            Some("two")
        );
        let first_line = 0..2;
        assert_eq!(
            state_for_selection(text, first_line, &defaults())
                .connection
                .database
                .as_deref(),
            Some("da")
        );
    }

    #[test]
    fn selected_text_takes_the_parameters_of_its_query() {
        let text = "//:setParam(\"raid\", \"one\")\n\nT1\n\n//:clearParams()\n\nT2 | take 1";
        let first = state_for_selection(text, 0..0, &defaults());
        assert_eq!(
            first.parameters,
            BTreeMap::from([("raid".to_string(), "one".to_string())]),
            "text above every query takes the parameters of the first query"
        );
        let selection = text.find("T2").unwrap()..text.len();
        assert!(
            state_for_selection(text, selection, &defaults())
                .parameters
                .is_empty()
        );
    }
}
