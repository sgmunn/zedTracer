//! Which columns of a trace table play which part.
//!
//! The activity view, the severity colours and the sequence view all read the same few
//! columns. Traces from other services name them differently, so every feature asks this
//! module for the columns instead of looking for names of its own.

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
        Self::resolve(table, &[])
    }

    /// Resolves the roles with the first schema that applies to the table, or with the
    /// built-in names when none does. A schema applies when it names no column the table
    /// lacks and every column it requires is present.
    pub fn resolve(table: &Table, schemas: &[TraceSchema]) -> Self {
        schemas
            .iter()
            .find_map(|schema| Self::from_schema(table, schema))
            .unwrap_or_else(|| Self::from_names(table, None))
    }

    fn from_schema(table: &Table, schema: &TraceSchema) -> Option<Self> {
        let requirements_met = schema
            .requires
            .iter()
            .all(|name| find_column(table, &[name.as_str()]).is_some());
        let names_exist = TraceRole::ALL.iter().all(|role| {
            schema
                .column_name(*role)
                .is_none_or(|name| find_column(table, &[name]).is_some())
        });
        (requirements_met && names_exist).then(|| Self::from_names(table, Some(schema)))
    }

    fn from_names(table: &Table, schema: Option<&TraceSchema>) -> Self {
        let resolve_role = |role: TraceRole| match schema.and_then(|schema| schema.column_name(role))
        {
            Some(name) => find_column(table, &[name]),
            None => find_column(table, role.built_in_names()),
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
            TraceColumns::resolve(&without, std::slice::from_ref(&schema)).marker,
            Some(1)
        );
        let with = table(&["SpanId", "MarkerName", "Operation"]);
        assert_eq!(TraceColumns::resolve(&with, &[schema]).marker, Some(2));
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
        let columns = TraceColumns::resolve(&table(&["Host", "Role"]), &[first, second]);
        assert_eq!(columns.actor, Some(1));
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
}
