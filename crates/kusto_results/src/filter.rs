//! Column filters: which operators a column offers and whether a cell satisfies them.
//!
//! A column has up to two conditions joined with "all" or "any". Matching is by type: text
//! operators ignore case, number and timespan operators compare values, datetime operators
//! compare instants at 100 ns resolution, with zone-less text read as UTC.

use crate::result::{Cell, ColumnKind};
use crate::typed::{
    NumberKey, cell_number, cell_ticks, parse_datetime_ticks, parse_number, parse_timespan_ticks,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FilterOperator {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    StartsWith,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
    IsEmpty,
    IsNotEmpty,
    IsTrue,
    IsFalse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperatorOption {
    pub operator: FilterOperator,
    pub label: &'static str,
    pub requires_value: bool,
}

const fn option(
    operator: FilterOperator,
    label: &'static str,
    requires_value: bool,
) -> OperatorOption {
    OperatorOption {
        operator,
        label,
        requires_value,
    }
}

const EMPTY_OPERATORS: [OperatorOption; 2] = [
    option(FilterOperator::IsEmpty, "Is empty", false),
    option(FilterOperator::IsNotEmpty, "Is not empty", false),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Text,
    Number,
    DateTime,
    Bool,
}

fn family(kind: ColumnKind) -> Family {
    match kind {
        ColumnKind::Bool => Family::Bool,
        ColumnKind::DateTime => Family::DateTime,
        ColumnKind::Int
        | ColumnKind::Long
        | ColumnKind::Real
        | ColumnKind::Decimal
        | ColumnKind::TimeSpan => Family::Number,
        ColumnKind::String | ColumnKind::Guid | ColumnKind::Dynamic | ColumnKind::Other => {
            Family::Text
        }
    }
}

/// The operators a filter popover offers for a column of this type, in display order.
pub fn operators_for(kind: ColumnKind) -> Vec<OperatorOption> {
    use FilterOperator::*;
    let mut options = match family(kind) {
        Family::Bool => vec![
            option(IsTrue, "Is true", false),
            option(IsFalse, "Is false", false),
        ],
        Family::Number => vec![
            option(Equals, "Equals", true),
            option(NotEquals, "Does not equal", true),
            option(GreaterThan, "Greater than", true),
            option(GreaterThanOrEqual, "Greater than or equal", true),
            option(LessThan, "Less than", true),
            option(LessThanOrEqual, "Less than or equal", true),
        ],
        Family::DateTime => vec![
            option(Equals, "On", true),
            option(NotEquals, "Not on", true),
            option(GreaterThan, "After", true),
            option(GreaterThanOrEqual, "On or after", true),
            option(LessThan, "Before", true),
            option(LessThanOrEqual, "On or before", true),
        ],
        Family::Text => vec![
            option(Contains, "Contains", true),
            option(NotContains, "Does not contain", true),
            option(Equals, "Equals", true),
            option(NotEquals, "Does not equal", true),
            option(StartsWith, "Starts with", true),
        ],
    };
    options.extend(EMPTY_OPERATORS);
    options
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub operator: FilterOperator,
    pub value: String,
}

impl Condition {
    pub fn new(operator: FilterOperator, value: impl Into<String>) -> Self {
        Self {
            operator,
            value: value.into(),
        }
    }

    /// A condition takes effect once its operator suits the column and, if the operator needs a
    /// value, a value has been typed.
    pub fn is_usable(&self, kind: ColumnKind) -> bool {
        operators_for(kind)
            .iter()
            .find(|option| option.operator == self.operator)
            .is_some_and(|option| !option.requires_value || !self.value.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    /// Every condition must match.
    #[default]
    All,
    /// Any condition may match.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnFilter {
    pub join: Join,
    pub conditions: Vec<Condition>,
}

impl ColumnFilter {
    /// The filter with its unusable conditions removed, or `None` when nothing is left, so a
    /// half-typed condition never hides rows.
    pub fn usable(&self, kind: ColumnKind) -> Option<ColumnFilter> {
        let conditions: Vec<Condition> = self
            .conditions
            .iter()
            .filter(|condition| condition.is_usable(kind))
            .cloned()
            .collect();
        (!conditions.is_empty()).then_some(ColumnFilter {
            join: self.join,
            conditions,
        })
    }

    pub fn matches(&self, cell: &Cell, kind: ColumnKind) -> bool {
        let mut results = self
            .conditions
            .iter()
            .map(|condition| condition_matches(cell, kind, condition));
        match self.join {
            Join::All => results.all(|matched| matched),
            Join::Any => results.any(|matched| matched),
        }
    }
}

fn condition_matches(cell: &Cell, kind: ColumnKind, condition: &Condition) -> bool {
    use FilterOperator::*;
    let text = cell.display_text();
    match condition.operator {
        IsEmpty => text.is_empty(),
        IsNotEmpty => !text.is_empty(),
        IsTrue => text.eq_ignore_ascii_case("true") || text == "1",
        IsFalse => text.eq_ignore_ascii_case("false") || text == "0",
        operator => match family(kind) {
            Family::Text | Family::Bool => text_matches(&text, operator, &condition.value),
            Family::Number => number_matches(cell, kind, operator, &condition.value),
            Family::DateTime => datetime_matches(cell, operator, &condition.value),
        },
    }
}

fn text_matches(text: &str, operator: FilterOperator, wanted: &str) -> bool {
    use FilterOperator::*;
    let folded = text.to_lowercase();
    let wanted = wanted.to_lowercase();
    match operator {
        Contains => folded.contains(&wanted),
        NotContains => !folded.contains(&wanted),
        Equals => folded == wanted,
        NotEquals => folded != wanted,
        StartsWith => folded.starts_with(&wanted),
        _ => false,
    }
}

fn number_matches(cell: &Cell, kind: ColumnKind, operator: FilterOperator, wanted: &str) -> bool {
    if cell.is_null() {
        return operator == FilterOperator::NotEquals;
    }
    let (actual, expected) = if kind == ColumnKind::TimeSpan {
        (
            cell_ticks(cell, kind).map(NumberKey::Int),
            parse_timespan_ticks(wanted).map(NumberKey::Int),
        )
    } else {
        (cell_number(cell, kind), parse_number(wanted, kind))
    };
    match (actual, expected) {
        (Some(actual), Some(expected)) => ordering_matches(operator, actual.compare(&expected)),
        _ => false,
    }
}

fn datetime_matches(cell: &Cell, operator: FilterOperator, wanted: &str) -> bool {
    if cell.is_null() {
        return operator == FilterOperator::NotEquals;
    }
    match (
        cell_ticks(cell, ColumnKind::DateTime),
        parse_datetime_ticks(wanted),
    ) {
        (Some(actual), Some(expected)) => ordering_matches(operator, actual.cmp(&expected)),
        _ => false,
    }
}

fn ordering_matches(operator: FilterOperator, ordering: std::cmp::Ordering) -> bool {
    use FilterOperator::*;
    use std::cmp::Ordering::*;
    match operator {
        Equals => ordering == Equal,
        NotEquals => ordering != Equal,
        GreaterThan => ordering == Greater,
        GreaterThanOrEqual => ordering != Less,
        LessThan => ordering == Less,
        LessThanOrEqual => ordering != Greater,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FilterOperator::*;

    fn filter(operator: FilterOperator, value: &str) -> ColumnFilter {
        ColumnFilter {
            join: Join::All,
            conditions: vec![Condition::new(operator, value)],
        }
    }

    fn text(value: &str) -> Cell {
        Cell::Text(value.to_string())
    }

    #[test]
    fn offers_operators_by_type() {
        let labels = |kind| -> Vec<&'static str> {
            operators_for(kind)
                .iter()
                .map(|option| option.label)
                .collect()
        };
        assert_eq!(
            labels(ColumnKind::String),
            [
                "Contains",
                "Does not contain",
                "Equals",
                "Does not equal",
                "Starts with",
                "Is empty",
                "Is not empty"
            ]
        );
        assert_eq!(operators_for(ColumnKind::Long).len(), 8);
        assert_eq!(labels(ColumnKind::TimeSpan), labels(ColumnKind::Long));
        assert_eq!(labels(ColumnKind::DateTime)[0], "On");
        assert_eq!(labels(ColumnKind::DateTime)[2], "After");
        assert_eq!(
            labels(ColumnKind::Bool),
            ["Is true", "Is false", "Is empty", "Is not empty"]
        );
        assert_eq!(labels(ColumnKind::Guid), labels(ColumnKind::Dynamic));
    }

    #[test]
    fn text_operators_ignore_case() {
        let cell = text("Timeout while reading");
        assert!(filter(Contains, "TIMEOUT").matches(&cell, ColumnKind::String));
        assert!(filter(StartsWith, "timeout").matches(&cell, ColumnKind::String));
        assert!(filter(NotContains, "retry").matches(&cell, ColumnKind::String));
        assert!(!filter(Equals, "timeout").matches(&cell, ColumnKind::String));
    }

    #[test]
    fn two_conditions_join_with_all_or_any() {
        let conditions = vec![
            Condition::new(Contains, "time"),
            Condition::new(Contains, "zzz"),
        ];
        let cell = text("timeout");
        let all = ColumnFilter {
            join: Join::All,
            conditions: conditions.clone(),
        };
        let any = ColumnFilter {
            join: Join::Any,
            conditions,
        };
        assert!(!all.matches(&cell, ColumnKind::String));
        assert!(any.matches(&cell, ColumnKind::String));
    }

    #[test]
    fn numbers_compare_by_value_and_null_never_orders() {
        let kind = ColumnKind::Long;
        let greater = filter(GreaterThan, "9");
        assert!(greater.matches(&Cell::Int(10), kind));
        assert!(!greater.matches(&Cell::Int(9), kind));
        assert!(!greater.matches(&Cell::Null, kind));
        assert!(!greater.matches(&text("abc"), kind));
        assert!(!filter(LessThan, "5").matches(&Cell::Null, kind));
        assert!(filter(NotEquals, "5").matches(&Cell::Null, kind));
        assert!(filter(IsEmpty, "").matches(&Cell::Null, kind));
    }

    #[test]
    fn integers_beyond_two_to_the_fifty_third_are_exact() {
        let cell = Cell::Int(9_007_199_254_740_993);
        assert!(filter(GreaterThan, "9007199254740992").matches(&cell, ColumnKind::Long));
    }

    #[test]
    fn timespans_compare_by_duration() {
        let kind = ColumnKind::TimeSpan;
        let under_a_minute = filter(LessThan, "00:01:00");
        assert!(under_a_minute.matches(&text("00:00:30.0000000"), kind));
        assert!(under_a_minute.matches(&text("-00:00:05"), kind));
        assert!(!under_a_minute.matches(&text("1.00:00:00"), kind));
        assert!(!under_a_minute.matches(&Cell::Null, kind));
    }

    #[test]
    fn datetimes_compare_instants_in_utc_to_the_tick() {
        let kind = ColumnKind::DateTime;
        let on = filter(Equals, "2025-01-01T00:00:00.0000000Z");
        assert!(on.matches(&text("2025-01-01T00:00:00.0000000Z"), kind));
        assert!(!on.matches(&text("2025-01-01T00:00:00.0000007Z"), kind));
        let after = filter(GreaterThan, "2025-01-01");
        assert!(after.matches(&text("2025-01-01T00:00:00.0000007Z"), kind));
        assert!(!after.matches(&text("2024-12-31T23:59:59.9999999Z"), kind));
        assert!(filter(NotEquals, "2025-01-01").matches(&Cell::Null, kind));
    }

    #[test]
    fn booleans_accept_words_and_digits() {
        let kind = ColumnKind::Bool;
        assert!(filter(IsTrue, "").matches(&Cell::Bool(true), kind));
        assert!(filter(IsTrue, "").matches(&text("1"), kind));
        assert!(filter(IsFalse, "").matches(&Cell::Bool(false), kind));
        assert!(!filter(IsFalse, "").matches(&Cell::Null, kind));
        assert!(filter(IsEmpty, "").matches(&Cell::Null, kind));
    }

    #[test]
    fn dynamic_values_match_against_compact_json() {
        let cell = Cell::Dynamic(Box::new(serde_json::json!({"callStack": "at A()"})));
        assert!(filter(Contains, "callstack").matches(&cell, ColumnKind::Dynamic));
    }

    #[test]
    fn half_typed_conditions_are_dropped() {
        let partial = ColumnFilter {
            join: Join::All,
            conditions: vec![Condition::new(Contains, ""), Condition::new(IsEmpty, "")],
        };
        let usable = partial.usable(ColumnKind::String);
        assert_eq!(usable.map(|filter| filter.conditions.len()), Some(1));
        assert!(filter(Contains, "").usable(ColumnKind::String).is_none());
        assert!(filter(IsTrue, "").usable(ColumnKind::String).is_none());
    }
}
