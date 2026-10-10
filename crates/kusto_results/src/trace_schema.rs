//! Which columns of a trace table play which part.
//!
//! The activity view, the severity colours and the sequence view all read the same few
//! columns. Traces from other services name them differently, so every feature asks this
//! module for the columns instead of looking for names of its own.

use std::collections::HashMap;

use crate::result::Table;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceRole {
    ActivityId,
    ParentActivityId,
    Marker,
    Actor,
    Timestamp,
    Severity,
    Message,
}

impl TraceRole {
    pub const ALL: [TraceRole; 7] = [
        TraceRole::ActivityId,
        TraceRole::ParentActivityId,
        TraceRole::Marker,
        TraceRole::Actor,
        TraceRole::Timestamp,
        TraceRole::Severity,
        TraceRole::Message,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TraceRole::ActivityId => "activity id",
            TraceRole::ParentActivityId => "parent activity id",
            TraceRole::Marker => "marker",
            TraceRole::Actor => "actor",
            TraceRole::Timestamp => "timestamp",
            TraceRole::Severity => "severity",
            TraceRole::Message => "message",
        }
    }

    /// The column names used when no schema names the role, tried in order.
    fn built_in_names(self) -> &'static [&'static str] {
        match self {
            TraceRole::ActivityId => &["CurrentActivityId"],
            TraceRole::ParentActivityId => &["ParentActivityId"],
            TraceRole::Marker => &["MarkerName"],
            TraceRole::Actor => &["ProcessName"],
            TraceRole::Timestamp => &["TIMESTAMP"],
            TraceRole::Severity => &["level", "severity"],
            TraceRole::Message => &["MessageText"],
        }
    }
}

/// Column names a user lists for each role, tried in order before the built-in names, for
/// traces that call a column something else (`MessageText` in one table, `EventText` in another).
pub type RoleNames = HashMap<TraceRole, Vec<String>>;

/// A user-defined mapping from roles to column names. A role it leaves out keeps its built-in
/// names.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraceSchema {
    pub name: String,
    /// Columns that must all be present for the schema to apply, so one list of schemas can
    /// serve several clusters.
    pub requires: Vec<String>,
    pub activity_id: Option<String>,
    pub parent_activity_id: Option<String>,
    pub marker: Option<String>,
    pub actor: Option<String>,
    pub timestamp: Option<String>,
    pub severity: Option<String>,
    pub message: Option<String>,
}

impl TraceSchema {
    fn column_name(&self, role: TraceRole) -> Option<&str> {
        match role {
            TraceRole::ActivityId => self.activity_id.as_deref(),
            TraceRole::ParentActivityId => self.parent_activity_id.as_deref(),
            TraceRole::Marker => self.marker.as_deref(),
            TraceRole::Actor => self.actor.as_deref(),
            TraceRole::Timestamp => self.timestamp.as_deref(),
            TraceRole::Severity => self.severity.as_deref(),
            TraceRole::Message => self.message.as_deref(),
        }
    }
}

/// The column index each role resolved to in one table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceColumns {
    pub activity_id: Option<usize>,
    pub parent_activity_id: Option<usize>,
    pub marker: Option<usize>,
    pub actor: Option<usize>,
    pub timestamp: Option<usize>,
    pub severity: Option<usize>,
    pub message: Option<usize>,
}

impl TraceColumns {
    /// Resolves the roles with the built-in names only.
    pub fn detect(table: &Table) -> Self {
        Self::resolve(table, &[], &RoleNames::new())
    }

    /// Resolves the roles with the first schema that applies to the table, or with the listed
    /// and then the built-in names when none does. A schema applies when it names no column the
    /// table lacks and every column it requires is present. A role the schema leaves out is
    /// looked for under `names` first, in order, and then under the built-in names.
    pub fn resolve(table: &Table, schemas: &[TraceSchema], names: &RoleNames) -> Self {
        schemas
            .iter()
            .find_map(|schema| Self::from_schema(table, schema, names))
            .unwrap_or_else(|| Self::from_names(table, None, names))
    }

    fn from_schema(table: &Table, schema: &TraceSchema, names: &RoleNames) -> Option<Self> {
        let requirements_met = schema
            .requires
            .iter()
            .all(|name| find_column(table, &[name.as_str()]).is_some());
        let names_exist = TraceRole::ALL.iter().all(|role| {
            schema
                .column_name(*role)
                .is_none_or(|name| find_column(table, &[name]).is_some())
        });
        (requirements_met && names_exist).then(|| Self::from_names(table, Some(schema), names))
    }

    fn from_names(table: &Table, schema: Option<&TraceSchema>, names: &RoleNames) -> Self {
        let resolve_role =
            |role: TraceRole| match schema.and_then(|schema| schema.column_name(role)) {
                Some(name) => find_column(table, &[name]),
                None => {
                    let listed = names.get(&role).map(Vec::as_slice).unwrap_or_default();
                    let candidates: Vec<&str> = listed
                        .iter()
                        .map(String::as_str)
                        .chain(role.built_in_names().iter().copied())
                        .collect();
                    find_column(table, &candidates)
                }
            };
        Self {
            activity_id: resolve_role(TraceRole::ActivityId),
            parent_activity_id: resolve_role(TraceRole::ParentActivityId),
            marker: resolve_role(TraceRole::Marker),
            actor: resolve_role(TraceRole::Actor),
            timestamp: resolve_role(TraceRole::Timestamp),
            severity: resolve_role(TraceRole::Severity),
            message: resolve_role(TraceRole::Message),
        }
    }

    pub fn column(&self, role: TraceRole) -> Option<usize> {
        match role {
            TraceRole::ActivityId => self.activity_id,
            TraceRole::ParentActivityId => self.parent_activity_id,
            TraceRole::Marker => self.marker,
            TraceRole::Actor => self.actor,
            TraceRole::Timestamp => self.timestamp,
            TraceRole::Severity => self.severity,
            TraceRole::Message => self.message,
        }
    }

    /// Whether the table has what the structured activity view needs.
    pub fn supports_activity(&self) -> bool {
        self.missing_for_activity().is_empty()
    }

    /// Whether the table has what the sequence view needs.
    pub fn supports_sequence(&self) -> bool {
        self.missing_for_sequence().is_empty()
    }

    pub fn missing_for_activity(&self) -> Vec<TraceRole> {
        self.missing(&[TraceRole::ActivityId, TraceRole::ParentActivityId])
    }

    pub fn missing_for_sequence(&self) -> Vec<TraceRole> {
        self.missing(&[
            TraceRole::ActivityId,
            TraceRole::ParentActivityId,
            TraceRole::Actor,
            TraceRole::Timestamp,
        ])
    }

    fn missing(&self, needed: &[TraceRole]) -> Vec<TraceRole> {
        needed
            .iter()
            .copied()
            .filter(|role| self.column(*role).is_none())
            .collect()
    }
}

/// Whether `text` matches a pattern as the trace settings write them: case does not matter and a
/// `*` at the start, the end or both stands for any text. Wrapper markers, routine warnings and
/// structural messages all use it.
pub fn matches_pattern(pattern: &str, text: &str) -> bool {
    let core = pattern.trim_matches('*').as_bytes();
    let text = text.as_bytes();
    let starts = pattern.starts_with('*');
    let ends = pattern.len() > 1 && pattern.ends_with('*');
    match (starts, ends) {
        (true, true) => {
            core.is_empty() || text.windows(core.len()).any(|part| part.eq_ignore_ascii_case(core))
        }
        (true, false) => {
            text.len() >= core.len() && text[text.len() - core.len()..].eq_ignore_ascii_case(core)
        }
        (false, true) => text.len() >= core.len() && text[..core.len()].eq_ignore_ascii_case(core),
        (false, false) => text.eq_ignore_ascii_case(core),
    }
}

/// Tries each name in turn, exact match first and then ignoring case. Padding around a column
/// name is ignored on both sides.
fn find_column(table: &Table, names: &[&str]) -> Option<usize> {
    names.iter().find_map(|name| {
        let name = name.trim();
        table
            .columns
            .iter()
            .position(|column| column.name.trim() == name)
            .or_else(|| {
                table
                    .columns
                    .iter()
                    .position(|column| column.name.trim().eq_ignore_ascii_case(name))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Column;

    fn table(names: &[&str]) -> Table {
        Table {
            name: "t".into(),
            columns: names
                .iter()
                .map(|name| Column::new(*name, "string"))
                .collect(),
            rows: Vec::new(),
        }
    }

    #[test]
    fn built_in_names_match_exactly_then_ignoring_case() {
        let columns = TraceColumns::detect(&table(&[
            "timestamp",
            "TIMESTAMP",
            "currentactivityid",
            "ParentActivityId",
            "Level",
        ]));
        assert_eq!(columns.timestamp, Some(1));
        assert_eq!(columns.activity_id, Some(2));
        assert_eq!(columns.parent_activity_id, Some(3));
        assert_eq!(columns.severity, Some(4));
        assert_eq!(columns.actor, None);
    }

    #[test]
    fn severity_prefers_level_over_severity() {
        let columns = TraceColumns::detect(&table(&["Severity", "Level"]));
        assert_eq!(columns.severity, Some(1));
    }

    #[test]
    fn a_schema_renames_roles_and_keeps_the_rest() {
        let schema = TraceSchema {
            name: "other".into(),
            activity_id: Some("SpanId".into()),
            parent_activity_id: Some("ParentSpanId".into()),
            actor: Some("Service".into()),
            ..TraceSchema::default()
        };
        let columns = TraceColumns::resolve(
            &table(&["SpanId", "ParentSpanId", "Service", "MarkerName", "TIMESTAMP"]),
            &[schema],
            &RoleNames::new(),
        );
        assert_eq!(columns.activity_id, Some(0));
        assert_eq!(columns.parent_activity_id, Some(1));
        assert_eq!(columns.actor, Some(2));
        assert_eq!(columns.marker, Some(3));
        assert_eq!(columns.timestamp, Some(4));
        assert!(columns.supports_sequence());
    }

    #[test]
    fn a_schema_naming_a_missing_column_is_skipped() {
        let wrong = TraceSchema {
            name: "wrong".into(),
            activity_id: Some("SpanId".into()),
            ..TraceSchema::default()
        };
        let columns = TraceColumns::resolve(
            &table(&["CurrentActivityId", "ParentActivityId"]),
            &[wrong],
            &RoleNames::new(),
        );
        assert_eq!(columns.activity_id, Some(0));
        assert!(columns.supports_activity());
    }

    #[test]
    fn a_schema_applies_only_when_its_required_columns_are_present() {
        let schema = TraceSchema {
            name: "cluster b".into(),
            requires: vec!["SpanId".into()],
            marker: Some("Operation".into()),
            ..TraceSchema::default()
        };
        let without = table(&["Operation", "MarkerName"]);
        assert_eq!(
            TraceColumns::resolve(&without, std::slice::from_ref(&schema), &RoleNames::new())
                .marker,
            Some(1)
        );
        let with = table(&["SpanId", "MarkerName", "Operation"]);
        assert_eq!(
            TraceColumns::resolve(&with, &[schema], &RoleNames::new()).marker,
            Some(2)
        );
    }

    #[test]
    fn the_first_schema_that_applies_wins() {
        let first = TraceSchema {
            name: "first".into(),
            actor: Some("Role".into()),
            ..TraceSchema::default()
        };
        let second = TraceSchema {
            name: "second".into(),
            actor: Some("Host".into()),
            ..TraceSchema::default()
        };
        let columns = TraceColumns::resolve(
            &table(&["Host", "Role"]),
            &[first, second],
            &RoleNames::new(),
        );
        assert_eq!(columns.actor, Some(1));
    }

    #[test]
    fn patterns_match_at_either_end_ignoring_case() {
        assert!(matches_pattern("*IncomingRequest", "WebApi-incomingrequest"));
        assert!(!matches_pattern("*IncomingRequest", "IncomingRequest-Extra"));
        assert!(matches_pattern("Monitored scope start*", "monitored scope start."));
        assert!(!matches_pattern("Monitored scope start*", "A monitored scope start"));
        assert!(matches_pattern("*request completed*", "Request completed in 5 ms"));
        assert!(matches_pattern("exact", "EXACT"));
        assert!(!matches_pattern("exact", "exactly"));
        assert!(matches_pattern("*", "anything"));
        assert!(matches_pattern("**", "anything"), "nothing but stars matches everything");
        assert!(!matches_pattern("longer than the text*", "short"));
        assert!(matches_pattern("*café*", "A CAFÉ and a café"), "only ASCII letters fold");
    }

    #[test]
    fn missing_roles_are_named() {
        let columns = TraceColumns::detect(&table(&["CurrentActivityId", "ParentActivityId"]));
        assert!(columns.supports_activity());
        assert_eq!(
            columns.missing_for_sequence(),
            vec![TraceRole::Actor, TraceRole::Timestamp]
        );
    }

    fn names(role: TraceRole, listed: &[&str]) -> RoleNames {
        RoleNames::from([(role, listed.iter().map(|name| name.to_string()).collect())])
    }

    fn resolved(table: &Table, names: &RoleNames) -> TraceColumns {
        TraceColumns::resolve(table, &[], names)
    }

    #[test]
    fn listed_names_are_tried_in_order_before_the_built_in_ones() {
        let both = table(&["MessageText", "EventText"]);
        let message_of =
            |listed: &[&str]| resolved(&both, &names(TraceRole::Message, listed)).message;
        assert_eq!(resolved(&both, &RoleNames::new()).message, Some(0));
        assert_eq!(message_of(&["EventText"]), Some(1));
        assert_eq!(
            message_of(&["Missing", "EventText", "MessageText"]),
            Some(1),
            "the first listed name that is in the table, not the first column"
        );
        assert_eq!(message_of(&["MessageText", "EventText"]), Some(0));
    }

    #[test]
    fn the_built_in_names_still_work_when_others_are_listed() {
        let only_built_in = table(&["MessageText"]);
        let listed = names(TraceRole::Message, &["EventText"]);
        assert_eq!(resolved(&only_built_in, &listed).message, Some(0));
    }

    #[test]
    fn listed_names_ignore_case_and_blank_padding() {
        let t = table(&["eventtext", " ActivityType "]);
        let mut listed = names(TraceRole::Message, &["EventText"]);
        listed.insert(TraceRole::Marker, vec!["ActivityType".into()]);
        let columns = resolved(&t, &listed);
        assert_eq!((columns.message, columns.marker), (Some(0), Some(1)));
    }

    #[test]
    fn severity_follows_the_order_it_is_listed_in() {
        let t = table(&["Level", "Severity"]);
        assert_eq!(
            resolved(&t, &RoleNames::new()).severity,
            Some(0),
            "level before severity"
        );
        let listed = names(TraceRole::Severity, &["Severity", "Level"]);
        assert_eq!(resolved(&t, &listed).severity, Some(1));
    }

    #[test]
    fn a_schema_wins_for_the_roles_it_names_and_listed_names_serve_the_rest() {
        let schema = TraceSchema {
            name: "spans".into(),
            actor: Some("Service".into()),
            ..TraceSchema::default()
        };
        let t = table(&["Service", "ProcessName", "ActivityType", "MarkerName"]);
        let listed = names(TraceRole::Marker, &["ActivityType"]);
        let columns = TraceColumns::resolve(&t, &[schema], &listed);
        assert_eq!(
            columns.actor,
            Some(0),
            "the schema's column, not ProcessName"
        );
        assert_eq!(columns.marker, Some(2), "the listed name, not MarkerName");
    }

    #[test]
    fn a_role_nothing_names_stays_unresolved() {
        let t = table(&["Something"]);
        let listed = names(TraceRole::Message, &["EventText"]);
        assert_eq!(resolved(&t, &listed).message, None);
    }
}
