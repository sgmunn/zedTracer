//! A query as it is copied, and as it is kept with its result: written so that it means the
//! same wherever it is run. The names that only its database knows are qualified by the language
//! server, which has the schema; here its parameters get the values they ran with, and a query
//! the server could not qualify says where it ran in a comment.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;

use kusto_results::export::string_literal;
use regex::Regex;
use serde::Deserialize;

use crate::directives::Connection;
use crate::query_text::is_control_command;

static DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdeclare\s+query_parameters\s*\(").expect("a valid pattern")
});

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("a valid pattern"));

static INTEGER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?[0-9]+$").expect("a valid pattern"));

static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?$").expect("a valid pattern")
});

static DATETIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}([T ][0-9]{2}:[0-9]{2}(:[0-9]{2}(\.[0-9]+)?)?)?Z?$")
        .expect("a valid pattern")
});

static TIMESPAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-?([0-9]+(\.[0-9]+)?(d|h|m|s|ms)|([0-9]+\.)?[0-9]{1,2}:[0-9]{2}(:[0-9]{2}(\.[0-9]+)?)?)$")
        .expect("a valid pattern")
});

static GUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$").expect("a valid pattern")
});

/// What the language server's `kusto.qualifyQuery` answered.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Qualified {
    pub text: String,
    /// Whether the server knew the schema of the query's database. When it did not, `text` is
    /// the query as it was.
    pub complete: bool,
}

/// The query for a copy, or to keep with a result: qualified by the language server when it
/// could, with the values its parameters ran with, and otherwise with the cluster and database
/// it ran on at its top, as the directives that set them. A control command is left as it is,
/// because the service does not take a comment before it.
pub fn portable_query(
    query: &str,
    connection: &Connection,
    values: &BTreeMap<String, String>,
    qualified: Option<&Qualified>,
) -> String {
    if is_control_command(query) {
        return query.to_string();
    }
    let qualified = qualified.filter(|qualified| qualified.complete);
    let text = with_parameter_values(
        qualified.map_or(query, |qualified| qualified.text.as_str()),
        values,
    );
    if qualified.is_some() {
        text
    } else {
        with_connection_comment(&text, connection)
    }
}

/// The query with `// :setDefaultCluster("…")` and `// :setDefaultDb("…")` above it, for the
/// parts of the connection there are. They are what a Kusto editor here reads, so the copy runs
/// where the original did when it is pasted into one.
pub fn with_connection_comment(query: &str, connection: &Connection) -> String {
    let mut text = String::new();
    if let Some(cluster) = &connection.cluster {
        text.push_str(&format!(
            "// :setDefaultCluster({})\n",
            string_literal(cluster)
        ));
    }
    if let Some(database) = &connection.database {
        text.push_str(&format!("// :setDefaultDb({})\n", string_literal(database)));
    }
    text.push_str(query);
    text
}

/// The query with each `declare query_parameters(...)` replaced by `let` statements that give the
/// parameters the values they ran with: the ones in `values`, else the defaults the declaration
/// gives. A parameter with neither stays declared, and so does one of a type that has no
/// value to give, such as a table.
pub fn with_parameter_values(query: &str, values: &BTreeMap<String, String>) -> String {
    let declarations = declarations(query);
    if declarations.is_empty() {
        return query.to_string();
    }
    let mut text = String::with_capacity(query.len());
    let mut copied = 0;
    for declaration in &declarations {
        text.push_str(&query[copied..declaration.range.start]);
        text.push_str(&replacement(
            declaration,
            &query[declaration.range.clone()],
            values,
        ));
        copied = declaration.range.end;
    }
    text.push_str(&query[copied..]);
    text
}

struct Declaration {
    range: Range<usize>,
    parameters: Vec<DeclaredParameter>,
}

struct DeclaredParameter {
    segment: String,
    name: String,
    type_name: String,
    default: Option<String>,
}

fn replacement(
    declaration: &Declaration,
    original: &str,
    values: &BTreeMap<String, String>,
) -> String {
    let mut lets = Vec::new();
    let mut unresolved = Vec::new();
    for parameter in &declaration.parameters {
        match literal_of(parameter, values) {
            Some(literal) => lets.push(format!("let {} = {literal};", parameter.name)),
            None => unresolved.push(parameter.segment.as_str()),
        }
    }
    if lets.is_empty() {
        return original.to_string();
    }
    let mut lines = Vec::new();
    if !unresolved.is_empty() {
        lines.push(format!(
            "declare query_parameters({});",
            unresolved.join(", ")
        ));
    }
    lines.extend(lets);
    lines.join("\n")
}

fn literal_of(parameter: &DeclaredParameter, values: &BTreeMap<String, String>) -> Option<String> {
    if !NAME.is_match(&parameter.name) {
        return None;
    }
    let type_name = parameter.type_name.to_ascii_lowercase();
    if let Some(value) = values.get(&parameter.name) {
        return value_literal(&type_name, value);
    }
    let default = parameter.default.as_deref()?;
    if is_number_type(&type_name) && NUMBER.is_match(default) {
        return value_literal(&type_name, default);
    }
    Some(default.to_string())
}

fn is_number_type(type_name: &str) -> bool {
    matches!(type_name, "long" | "int" | "real" | "double" | "decimal")
}

/// A literal of a type for a value written as text. A value that is not written the way the type
/// is written in a query goes through the conversion function of the type instead, which takes any
/// text, so the copy is always a query, and says what the value was.
fn value_literal(type_name: &str, value: &str) -> Option<String> {
    if type_name == "string" {
        return Some(string_literal(value));
    }
    let value = value.trim();
    let converted = |function: &str| format!("{function}({})", string_literal(value));
    Some(match type_name {
        "long" | "int" if INTEGER.is_match(value) => format!("{type_name}({value})"),
        "long" => converted("tolong"),
        "int" => converted("toint"),
        "real" | "double" if NUMBER.is_match(value) => format!("real({value})"),
        "real" | "double" => converted("toreal"),
        "decimal" if NUMBER.is_match(value) => format!("decimal({value})"),
        "decimal" => converted("todecimal"),
        "bool" | "boolean" if value.eq_ignore_ascii_case("true") => "true".to_string(),
        "bool" | "boolean" if value.eq_ignore_ascii_case("false") => "false".to_string(),
        "bool" | "boolean" => converted("tobool"),
        "datetime" | "date" if DATETIME.is_match(value) => format!("datetime({value})"),
        "datetime" | "date" => converted("todatetime"),
        "timespan" | "time" if TIMESPAN.is_match(value) => format!("timespan({value})"),
        "timespan" | "time" => converted("totimespan"),
        "guid" | "uuid" | "uniqueid" if GUID.is_match(value) => format!("guid({value})"),
        "guid" | "uuid" | "uniqueid" => converted("toguid"),
        "dynamic" => converted("todynamic"),
        _ => return None,
    })
}

fn declarations(query: &str) -> Vec<Declaration> {
    let parts = parts_of(query);
    let mut declarations = Vec::new();
    let mut resume_at = 0;
    for found in DECLARATION.find_iter(query) {
        if found.start() < resume_at || parts[found.start()] != Part::Code {
            continue;
        }
        let Some((segments, list_end)) = parameter_segments(query, &parts, found.end()) else {
            continue;
        };
        let after_list = query[list_end..].trim_start();
        let end = if after_list.starts_with(';') {
            query.len() - after_list.len() + 1
        } else {
            list_end
        };
        resume_at = end;
        declarations.push(Declaration {
            range: found.start()..end,
            parameters: segments.into_iter().map(parameter_of).collect(),
        });
    }
    declarations
}

/// The comma-separated parts of a parameter list that starts at `start`, just after its opening
/// parenthesis, without their comments, and where the list ends, after its closing parenthesis.
fn parameter_segments(query: &str, parts: &[Part], start: usize) -> Option<(Vec<String>, usize)> {
    let segment = |from: usize, to: usize| {
        let bytes = (from..to)
            .filter(|at| parts[*at] != Part::Comment)
            .map(|at| query.as_bytes()[at])
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).trim().to_string()
    };
    let mut depth = 1;
    let mut segment_start = start;
    let mut segments = Vec::new();
    for (offset, byte) in query.as_bytes()[start..].iter().enumerate() {
        let at = start + offset;
        if parts[at] != Part::Code {
            continue;
        }
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    segments.push(segment(segment_start, at));
                    return Some((segments, at + 1));
                }
            }
            b',' if depth == 1 => {
                segments.push(segment(segment_start, at));
                segment_start = at + 1;
            }
            _ => {}
        }
    }
    None
}

fn parameter_of(segment: String) -> DeclaredParameter {
    let (name, rest) = segment.split_once(':').unwrap_or((&segment, ""));
    let (type_name, default) = match rest.split_once('=') {
        Some((type_name, default)) => (type_name.trim(), Some(default.trim().to_string())),
        None => (rest.trim(), None),
    };
    DeclaredParameter {
        name: name.trim().to_string(),
        type_name: type_name.to_string(),
        default: default.filter(|default| !default.is_empty()),
        segment,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Code,
    String,
    Comment,
}

/// For each byte of the query, what it is part of.
fn parts_of(query: &str) -> Vec<Part> {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        Comment,
        String { quote: char, verbatim: bool },
    }

    let mut parts = vec![Part::Code; query.len()];
    let mut state = State::Code;
    let mut previous = None;
    let mut characters = query.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        let length = character.len_utf8();
        match state {
            State::Code => match character {
                '/' if characters.peek().is_some_and(|(_, next)| *next == '/') => {
                    parts[offset..offset + length].fill(Part::Comment);
                    state = State::Comment;
                }
                '\'' | '"' => {
                    parts[offset..offset + length].fill(Part::String);
                    state = State::String {
                        quote: character,
                        verbatim: previous == Some('@'),
                    };
                }
                _ => {}
            },
            State::Comment => {
                if character == '\n' {
                    state = State::Code;
                } else {
                    parts[offset..offset + length].fill(Part::Comment);
                }
            }
            State::String { quote, verbatim } => {
                parts[offset..offset + length].fill(Part::String);
                if character == '\\' && !verbatim {
                    if let Some((next_offset, next)) = characters.next() {
                        parts[next_offset..next_offset + next.len_utf8()].fill(Part::String);
                    }
                } else if character == quote {
                    if verbatim && characters.peek().is_some_and(|(_, next)| *next == quote) {
                        if let Some((next_offset, next)) = characters.next() {
                            parts[next_offset..next_offset + next.len_utf8()].fill(Part::String);
                        }
                    } else {
                        state = State::Code;
                    }
                }
            }
        }
        previous = Some(character);
    }
    parts
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn connection() -> Connection {
        Connection {
            cluster: Some("https://help.kusto.windows.net".into()),
            database: Some("Samples".into()),
        }
    }

    #[test]
    fn a_declaration_becomes_lets_with_the_values_of_the_run() {
        let query = "declare query_parameters(raid:string, count:long);\nT\n| where Id == raid\n| take count";
        assert_eq!(
            with_parameter_values(query, &values(&[("raid", "abc-123"), ("count", "5")])),
            "let raid = 'abc-123';\nlet count = long(5);\nT\n| where Id == raid\n| take count"
        );
    }

    #[test]
    fn a_value_is_written_as_a_literal_of_its_type() {
        let query = "declare query_parameters(a:bool, b:datetime, c:timespan, d:guid, e:real, f:int, g:decimal, h:dynamic);\nprint 1";
        let all = values(&[
            ("a", "TRUE"),
            ("b", "2026-10-07T01:02:03Z"),
            ("c", "1.5h"),
            ("d", "6066d1fb-4746-469c-a460-2ea1defd5642"),
            ("e", "2"),
            ("f", "-4"),
            ("g", "1.25"),
            ("h", "{\"a\":1}"),
        ]);
        assert_eq!(
            with_parameter_values(query, &all),
            [
                "let a = true;",
                "let b = datetime(2026-10-07T01:02:03Z);",
                "let c = timespan(1.5h);",
                "let d = guid(6066d1fb-4746-469c-a460-2ea1defd5642);",
                "let e = real(2);",
                "let f = int(-4);",
                "let g = decimal(1.25);",
                "let h = todynamic('{\"a\":1}');",
                "print 1",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_value_that_is_not_written_like_its_type_goes_through_the_conversion_function() {
        let query = "declare query_parameters(n:long, when:datetime, flag:bool);\nprint 1";
        assert_eq!(
            with_parameter_values(
                query,
                &values(&[("n", "many"), ("when", "yesterday"), ("flag", "maybe")])
            ),
            "let n = tolong('many');\nlet when = todatetime('yesterday');\nlet flag = tobool('maybe');\nprint 1"
        );
    }

    #[test]
    fn a_string_is_quoted_so_that_it_cannot_change_the_query() {
        let query = "declare query_parameters(name:string);\nprint name";
        assert_eq!(
            with_parameter_values(query, &values(&[("name", "it's \"x\"; drop\n")])),
            "let name = \"it's \\\"x\\\"; drop\\n\";\nprint name"
        );
        assert_eq!(
            with_parameter_values(query, &values(&[("name", "a'); print ('b")])),
            "let name = \"a'); print ('b\";\nprint name"
        );
    }

    #[test]
    fn a_default_is_the_value_when_there_is_no_other() {
        let query = "declare query_parameters(raid:string = 'none', limit:long = 10, ratio:real = 1, since:datetime = datetime(2020-01-01));\nprint 1";
        assert_eq!(
            with_parameter_values(query, &values(&[("raid", "x")])),
            "let raid = 'x';\nlet limit = long(10);\nlet ratio = real(1);\nlet since = datetime(2020-01-01);\nprint 1"
        );
        assert_eq!(
            with_parameter_values(query, &BTreeMap::new()),
            "let raid = 'none';\nlet limit = long(10);\nlet ratio = real(1);\nlet since = datetime(2020-01-01);\nprint 1"
        );
    }

    #[test]
    fn a_parameter_with_no_value_stays_declared() {
        let query = "declare query_parameters(raid:string, count:long);\nprint 1";
        assert_eq!(
            with_parameter_values(query, &values(&[("count", "5")])),
            "declare query_parameters(raid:string);\nlet count = long(5);\nprint 1"
        );
        assert_eq!(with_parameter_values(query, &BTreeMap::new()), query);
    }

    #[test]
    fn a_table_parameter_stays_declared() {
        let query = "declare query_parameters(rows:(id:long, name:string), n:long);\nprint 1";
        assert_eq!(
            with_parameter_values(query, &values(&[("rows", "x"), ("n", "1")])),
            "declare query_parameters(rows:(id:long, name:string));\nlet n = long(1);\nprint 1"
        );
    }

    #[test]
    fn what_is_not_a_declaration_is_left_alone() {
        let query = "// declare query_parameters(a:string);\nprint 'declare query_parameters(a:string);'\n| take 1";
        assert_eq!(with_parameter_values(query, &values(&[("a", "x")])), query);
        assert_eq!(
            with_parameter_values("print 1", &values(&[("a", "x")])),
            "print 1"
        );
    }

    #[test]
    fn a_declaration_is_found_whatever_its_case_layout_and_comments() {
        let query = "// the incident\nDECLARE   Query_Parameters (\n    raid : string, // which one\n    n:long = 3\n) ;\nprint raid, n";
        assert_eq!(
            with_parameter_values(query, &values(&[("raid", "x")])),
            "// the incident\nlet raid = 'x';\nlet n = long(3);\nprint raid, n"
        );
    }

    #[test]
    fn a_default_with_parentheses_and_commas_in_a_string_is_one_default() {
        let query = "declare query_parameters(a:string = 'x, (y)', b:long);\nprint 1";
        assert_eq!(
            with_parameter_values(query, &values(&[("b", "2")])),
            "let a = 'x, (y)';\nlet b = long(2);\nprint 1"
        );
    }

    #[test]
    fn a_connection_comment_names_the_cluster_and_database_as_directives() {
        assert_eq!(
            with_connection_comment("T\n| take 1", &connection()),
            "// :setDefaultCluster('https://help.kusto.windows.net')\n// :setDefaultDb('Samples')\nT\n| take 1"
        );
        assert_eq!(
            with_connection_comment(
                "T",
                &Connection {
                    cluster: None,
                    database: Some("Samples".into())
                }
            ),
            "// :setDefaultDb('Samples')\nT"
        );
        assert_eq!(with_connection_comment("T", &Connection::default()), "T");
    }

    #[test]
    fn a_query_the_server_qualified_is_not_given_a_comment() {
        let qualified = Qualified {
            text: "cluster('help').database('Samples').T".into(),
            complete: true,
        };
        assert_eq!(
            portable_query("T", &connection(), &BTreeMap::new(), Some(&qualified)),
            "cluster('help').database('Samples').T"
        );
    }

    #[test]
    fn a_query_the_server_could_not_qualify_says_where_it_ran() {
        let incomplete = Qualified {
            text: "T".into(),
            complete: false,
        };
        let expected = "// :setDefaultCluster('https://help.kusto.windows.net')\n// :setDefaultDb('Samples')\nT";
        assert_eq!(
            portable_query("T", &connection(), &BTreeMap::new(), Some(&incomplete)),
            expected
        );
        assert_eq!(
            portable_query("T", &connection(), &BTreeMap::new(), None),
            expected
        );
    }

    #[test]
    fn the_values_are_put_in_whether_or_not_the_server_qualified() {
        let query = "declare query_parameters(raid:string);\nT | where Id == raid";
        let qualified = Qualified {
            text: "declare query_parameters(raid:string);\ncluster('help').database('Samples').T | where Id == raid".into(),
            complete: true,
        };
        assert_eq!(
            portable_query(
                query,
                &connection(),
                &values(&[("raid", "x")]),
                Some(&qualified)
            ),
            "let raid = 'x';\ncluster('help').database('Samples').T | where Id == raid"
        );
    }

    #[test]
    fn a_control_command_is_left_as_it_is() {
        assert_eq!(
            portable_query(".show tables", &connection(), &BTreeMap::new(), None),
            ".show tables"
        );
    }
}
