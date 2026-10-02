//! Query parameter profiles: named sets of values for the parameters a query declares with
//! `declare query_parameters(...)`, kept in YAML next to the queries.
//!
//! The values travel as native Kusto query parameters. Nothing is substituted into the query
//! text, so a value cannot change what the query means.
//!
//! ```yaml
//! active: Incident A
//! profiles:
//!   Incident A:
//!     raid: abc-123
//!   Incident B:
//!     raid: def-456
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use regex::Regex;
use serde_yaml_ng::Value;

/// The folder, at the root of a project, that holds the profiles every query of it shares.
pub const WORKSPACE_PARAMETERS_PATH: &str = ".kusto/parameters.yaml";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParameterProfiles {
    pub active: Option<String>,
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

static PARAMETER_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("a valid pattern"));

static ACTIVE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^active[ \t]*:.*$").expect("a valid pattern"));

static DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdeclare\s+query_parameters\s*\(").expect("a valid pattern")
});

impl ParameterProfiles {
    /// Reads a profiles file. An empty file has no profiles. A profile's values that are not
    /// text, numbers or booleans, and names that cannot be a parameter, are left out.
    pub fn parse(text: &str) -> Result<Self> {
        let document: Value =
            serde_yaml_ng::from_str(text).map_err(|error| anyhow!("not valid YAML: {error}"))?;
        let root = match document {
            Value::Null => return Ok(Self::default()),
            Value::Mapping(root) => root,
            _ => {
                return Err(anyhow!(
                    "expected an `active` profile and a `profiles` mapping"
                ));
            }
        };

        let profiles: Vec<Profile> = match root.get("profiles") {
            Some(Value::Mapping(profiles)) => profiles
                .iter()
                .filter_map(|(name, values)| {
                    let name = scalar_text(name)?;
                    let values = values.as_mapping()?;
                    Some(Profile {
                        name,
                        values: values
                            .iter()
                            .filter_map(|(key, value)| {
                                let key = scalar_text(key)?;
                                PARAMETER_NAME
                                    .is_match(&key)
                                    .then(|| Some((key, scalar_text(value)?)))
                                    .flatten()
                            })
                            .collect(),
                    })
                })
                .collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(_) => return Err(anyhow!("`profiles` must be a mapping of names to values")),
        };
        let active = root
            .get("active")
            .and_then(Value::as_str)
            .filter(|name| profiles.iter().any(|profile| profile.name == *name))
            .map(str::to_string);
        Ok(Self { active, profiles })
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        let active = self.active.as_deref()?;
        self.profiles.iter().find(|profile| profile.name == active)
    }
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// The text of a profiles file with another profile active, or with none for `None`. The result
/// is `None` when the file has no such profile. Only the `active` line changes, so comments and
/// layout stay.
pub fn with_active_profile(text: &str, name: Option<&str>) -> Option<String> {
    let profiles = ParameterProfiles::parse(text).ok()?;
    if let Some(name) = name
        && !profiles.profiles.iter().any(|profile| profile.name == name)
    {
        return None;
    }
    let line = match name {
        Some(name) => format!("active: {}", serde_json::to_string(name).ok()?),
        None => "active: null".to_string(),
    };
    Some(if ACTIVE_LINE.is_match(text) {
        ACTIVE_LINE
            .replace(text, regex::NoExpand(&line))
            .into_owned()
    } else {
        format!("{line}\n{text}")
    })
}

/// A starting point for a profiles file: the profiles it was made from, or one example.
pub fn template(from: &ParameterProfiles) -> String {
    let mut text = String::from(
        "# Values for the parameters that queries declare with\n# declare query_parameters(name:type);\n",
    );
    if from.profiles.is_empty() {
        text.push_str("active: Example\nprofiles:\n  Example:\n    name: value\n");
        return text;
    }
    if let Some(active) = &from.active {
        text.push_str(&format!("active: {}\n", yaml_text(active)));
    }
    text.push_str("profiles:\n");
    for profile in &from.profiles {
        text.push_str(&format!("  {}:\n", yaml_text(&profile.name)));
        for (name, value) in &profile.values {
            text.push_str(&format!("    {name}: {}\n", yaml_text(value)));
        }
    }
    text
}

fn yaml_text(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
}

/// Where a query file's own profiles live: `queries/incident.kql` has `queries/incident.parameters.yaml`.
pub fn sidecar_path(query_file: &Path) -> Option<PathBuf> {
    let name = query_file.file_name()?.to_str()?;
    let stem = name.strip_suffix(".kql")?;
    Some(query_file.with_file_name(format!("{stem}.parameters.yaml")))
}

/// The names a query declares with `declare query_parameters(...)`.
pub fn declared_parameters(query: &str) -> Vec<String> {
    let text = without_comments(query);
    let mut names = Vec::new();
    for found in DECLARATION.find_iter(&text) {
        let mut depth = 1;
        let mut quote: Option<char> = None;
        let mut segment_start = found.end();
        let mut segments = Vec::new();
        for (offset, character) in text[found.end()..].char_indices() {
            let at = found.end() + offset;
            if let Some(open) = quote {
                if character == open {
                    quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' => quote = Some(character),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        segments.push(&text[segment_start..at]);
                        break;
                    }
                }
                ',' if depth == 1 => {
                    segments.push(&text[segment_start..at]);
                    segment_start = at + 1;
                }
                _ => {}
            }
        }
        for segment in segments {
            let name: String = segment
                .trim_start()
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if PARAMETER_NAME.is_match(&name) && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

fn without_comments(query: &str) -> String {
    query
        .lines()
        .map(|line| {
            let mut quote: Option<char> = None;
            let mut previous = None;
            for (offset, character) in line.char_indices() {
                match quote {
                    Some(open) if character == open => quote = None,
                    Some(_) => {}
                    None if character == '/' && previous == Some('/') => {
                        return &line[..offset - 1];
                    }
                    None if character == '\'' || character == '"' => quote = Some(character),
                    None => {}
                }
                previous = Some(character);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The values for the parameters a query declares, from the active profile. A declared
/// parameter the profile has no value for is left out, which is right when the declaration
/// gives it a default and otherwise is for the service to complain about.
pub fn parameters_for_query(query: &str, profile: Option<&Profile>) -> BTreeMap<String, String> {
    let Some(profile) = profile else {
        return BTreeMap::new();
    };
    declared_parameters(query)
        .into_iter()
        .filter_map(|name| {
            let value = profile.values.get(&name)?.clone();
            Some((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# the incidents\nactive: Incident B\nprofiles:\n  Incident A:\n    raid: abc-123\n    environment: prod\n  Incident B:\n    raid: def-456\n    count: 5\n    verbose: true\n    skipped: [1, 2]\n    not a name: x\n";

    fn profile(name: &str, values: &[(&str, &str)]) -> Profile {
        Profile {
            name: name.into(),
            values: values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_file_gives_its_profiles_in_order_and_the_active_one() {
        let profiles = ParameterProfiles::parse(FILE).expect("a valid file");
        assert_eq!(
            profiles.profiles,
            [
                profile(
                    "Incident A",
                    &[("raid", "abc-123"), ("environment", "prod")]
                ),
                profile(
                    "Incident B",
                    &[("raid", "def-456"), ("count", "5"), ("verbose", "true")]
                ),
            ]
        );
        assert_eq!(profiles.active.as_deref(), Some("Incident B"));
        assert_eq!(
            profiles
                .active_profile()
                .map(|profile| profile.name.as_str()),
            Some("Incident B")
        );
    }

    #[test]
    fn an_active_profile_that_does_not_exist_is_no_active_profile() {
        let profiles = ParameterProfiles::parse("active: Gone\nprofiles:\n  Here:\n    a: b\n")
            .expect("a valid file");
        assert_eq!(profiles.active, None);
        assert!(profiles.active_profile().is_none());
    }

    #[test]
    fn an_empty_file_has_no_profiles_and_a_broken_one_says_why() {
        assert_eq!(
            ParameterProfiles::parse("").expect("empty"),
            ParameterProfiles::default()
        );
        assert_eq!(
            ParameterProfiles::parse("# only a note\n").expect("empty"),
            ParameterProfiles::default()
        );
        assert!(ParameterProfiles::parse("profiles: [a, b]").is_err());
        assert!(ParameterProfiles::parse("- a\n- b").is_err());
        assert!(ParameterProfiles::parse("active: [").is_err());
    }

    #[test]
    fn making_a_profile_active_changes_only_the_active_line() {
        let changed = with_active_profile(FILE, Some("Incident A")).expect("the profile exists");
        assert_eq!(
            changed,
            FILE.replace("active: Incident B", "active: \"Incident A\"")
        );
        assert_eq!(
            ParameterProfiles::parse(&changed)
                .expect("still valid")
                .active
                .as_deref(),
            Some("Incident A")
        );
    }

    #[test]
    fn a_file_with_no_active_line_gets_one_first() {
        let text = "profiles:\n  One:\n    a: b\n";
        assert_eq!(
            with_active_profile(text, Some("One")).as_deref(),
            Some("active: \"One\"\nprofiles:\n  One:\n    a: b\n")
        );
    }

    #[test]
    fn no_profile_can_be_made_active() {
        let changed = with_active_profile(FILE, None).expect("a valid file");
        assert_eq!(changed, FILE.replace("active: Incident B", "active: null"));
        assert_eq!(
            ParameterProfiles::parse(&changed)
                .expect("still valid")
                .active,
            None
        );
    }

    #[test]
    fn an_unknown_profile_cannot_be_made_active() {
        assert_eq!(with_active_profile(FILE, Some("Incident C")), None);
        assert_eq!(with_active_profile("active: [", Some("x")), None);
    }

    #[test]
    fn the_template_keeps_what_it_is_made_from() {
        let from = ParameterProfiles::parse(FILE).expect("a valid file");
        let text = template(&from);
        let again = ParameterProfiles::parse(&text).expect("the template is valid");
        assert_eq!(again.profiles, from.profiles);
        assert_eq!(again.active, from.active);

        let example = ParameterProfiles::parse(&template(&ParameterProfiles::default()))
            .expect("the example is valid");
        assert_eq!(example.profiles.len(), 1);
        assert!(example.active_profile().is_some());
    }

    #[test]
    fn a_query_file_has_a_sidecar_beside_it() {
        assert_eq!(
            sidecar_path(Path::new("/work/queries/incident.kql")),
            Some(PathBuf::from("/work/queries/incident.parameters.yaml"))
        );
        assert_eq!(sidecar_path(Path::new("/work/notes.txt")), None);
    }

    #[test]
    fn declared_parameters_are_found_whatever_their_types_and_defaults() {
        let query = "declare query_parameters(raid:string, since:datetime = datetime(2024-01-01, 5), label:string = \"a,b)\");\nT | where X == raid";
        assert_eq!(declared_parameters(query), ["raid", "since", "label"]);
    }

    #[test]
    fn declarations_in_comments_and_repeated_declarations_are_handled() {
        let query = "// declare query_parameters(old:string);\nDECLARE  Query_Parameters ( one : long );\ndeclare query_parameters(two:long, one:long);\nT";
        assert_eq!(declared_parameters(query), ["one", "two"]);
        assert!(declared_parameters("T | take 1").is_empty());
        assert!(declared_parameters("declare query_parameters(").is_empty());
    }

    #[test]
    fn the_shared_cases_declare_the_same_parameters_as_the_language_server_finds() {
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            query: String,
            names: Vec<String>,
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fork-docs/samples/query-parameters.json"
        );
        let cases: Vec<Case> =
            serde_json::from_str(&std::fs::read_to_string(path).expect("the cases file reads"))
                .expect("the cases file parses");
        assert!(!cases.is_empty());
        for case in cases {
            assert_eq!(
                declared_parameters(&case.query),
                case.names,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn a_quoted_double_slash_is_not_a_comment() {
        let query = "declare query_parameters(url:string = \"http://x\", other:string);\nT";
        assert_eq!(declared_parameters(query), ["url", "other"]);
    }

    #[test]
    fn only_the_declared_parameters_the_profile_has_are_sent() {
        let profile = profile("P", &[("raid", "abc"), ("unused", "x")]);
        let query = "declare query_parameters(raid:string, missing:long);\nT";
        assert_eq!(
            parameters_for_query(query, Some(&profile)),
            BTreeMap::from([("raid".to_string(), "abc".to_string())])
        );
        assert!(parameters_for_query("T | take 1", Some(&profile)).is_empty());
        assert!(parameters_for_query(query, None).is_empty());
    }
}
