//! Which activities of a trace failed, and where each failure began.
//!
//! An activity's error is its own when it ended in that error. If it logged an error and then
//! finished normally it recovered, so the error is *handled* and does not make anything fail.
//! A failure that passes up through callers is logged again by each of them, so only the
//! activity with no failing activity below it is where the failure began: its *origin*.

use crate::activity::{ActivityProjection, Strength};
use crate::result::Table;
use crate::timeline::Timeline;
use crate::trace_text::is_filler;
use crate::trace_schema::TraceColumns;
use crate::view::severity_level;

#[derive(Debug, Clone, PartialEq)]
pub struct Failures {
    /// The row showing an activity's own error, when the activity ended in it.
    pub error_row: Vec<Option<usize>>,
    /// The row of an error an activity logged and then recovered from.
    pub handled_row: Vec<Option<usize>>,
    /// Whether this activity or one below it ended in an error.
    pub subtree_error: Vec<bool>,
    /// Whether an activity below this one ended in an error. This activity's own error is then
    /// the same failure passing through.
    pub descendant_error: Vec<bool>,
}

impl Failures {
    pub fn analyze(
        table: &Table,
        projection: &ActivityProjection,
        columns: &TraceColumns,
        timeline: &Timeline,
    ) -> Self {
        let count = projection.activities.len();
        let mut failures = Self {
            error_row: vec![None; count],
            handled_row: vec![None; count],
            subtree_error: vec![false; count],
            descendant_error: vec![false; count],
        };
        for (index, activity) in projection.activities.iter().enumerate() {
            let Some(row) = first_error_row(table, columns, &activity.event_rows) else {
                continue;
            };
            let recovered = activity
                .severity
                .is_some_and(|severity| severity.strength == Strength::Muted);
            if recovered {
                failures.handled_row[index] = Some(row);
            } else {
                failures.error_row[index] = Some(row);
                failures.subtree_error[index] = true;
            }
        }
        // Children are pushed after their parent, so walking the order backwards visits every
        // child before its parent.
        let mut preorder = Vec::with_capacity(count);
        let mut pending: Vec<usize> = timeline.roots.iter().rev().copied().collect();
        while let Some(activity) = pending.pop() {
            preorder.push(activity);
            pending.extend(timeline.children[activity].iter().rev().copied());
        }
        for activity in preorder.into_iter().rev() {
            if !failures.subtree_error[activity] {
                continue;
            }
            if let Some(parent) = projection.activities[activity].parent {
                failures.subtree_error[parent] = true;
                failures.descendant_error[parent] = true;
            }
        }
        failures
    }

    /// Whether the failure began in this activity.
    pub fn is_origin(&self, activity: usize) -> bool {
        self.error_row[activity].is_some() && !self.descendant_error[activity]
    }

    pub fn origins(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.error_row.len()).filter(|activity| self.is_origin(*activity))
    }
}

/// The text of a row's message.
pub(crate) fn row_message(table: &Table, columns: &TraceColumns, row: usize) -> Option<String> {
    let column = columns.message?;
    Some(table.cell(row, column)?.display_text().into_owned())
}

fn first_error_row(table: &Table, columns: &TraceColumns, rows: &[usize]) -> Option<usize> {
    let severity = columns.severity?;
    rows.iter().copied().find(|row| {
        severity_level(table.cell(*row, severity)).is_some_and(|level| level <= 2)
            && !row_message(table, columns, *row).is_some_and(|text| is_filler(&text))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::build_projection_with;
    use crate::result::{Cell, Column};
    use crate::timeline::TimelineOptions;

    fn trace(events: &[(&str, &str, i64, &str)]) -> Table {
        Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("Level", "long"),
                Column::new("MessageText", "string"),
                Column::new("TIMESTAMP", "datetime"),
            ],
            rows: events
                .iter()
                .enumerate()
                .map(|(index, (activity, parent, level, message))| {
                    let text = |value: &str| {
                        if value.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Text(value.into())
                        }
                    };
                    vec![
                        text(activity),
                        text(parent),
                        Cell::Int(*level),
                        Cell::Text((*message).into()),
                        Cell::Text(format!("2026-01-01T00:00:00.{:07}Z", index * 10_000)),
                    ]
                })
                .collect(),
        }
    }

    fn analyze(table: &Table) -> (ActivityProjection, Failures) {
        let columns = TraceColumns::detect(table);
        let projection = build_projection_with(table, &columns).expect("a projection");
        let timeline = Timeline::build(table, &projection, &columns, &TimelineOptions::default());
        let failures = Failures::analyze(table, &projection, &columns, &timeline);
        (projection, failures)
    }

    fn index(projection: &ActivityProjection, id: &str) -> usize {
        projection
            .activities
            .iter()
            .position(|activity| activity.activity_id == id)
            .expect("the activity exists")
    }

    #[test]
    fn a_failure_began_where_nothing_below_it_failed() {
        let (projection, failures) = analyze(&trace(&[
            ("r", "", 4, "start"),
            ("a", "r", 2, "outer failed"),
            ("b", "a", 2, "inner failed"),
            ("r", "", 2, "run failed"),
        ]));
        let origin = index(&projection, "b");
        assert_eq!(failures.origins().collect::<Vec<_>>(), vec![origin]);
        assert!(failures.is_origin(origin));
        assert!(!failures.is_origin(index(&projection, "a")), "a logged the same failure again");
        assert!(failures.subtree_error[index(&projection, "r")]);
        assert!(failures.descendant_error[index(&projection, "a")]);
        assert!(!failures.descendant_error[origin]);
    }

    #[test]
    fn an_activity_that_logged_an_error_and_ended_normally_recovered() {
        let (projection, failures) = analyze(&trace(&[
            ("r", "", 4, "start"),
            ("a", "r", 2, "retrying"),
            ("a", "r", 4, "worked"),
        ]));
        let a = index(&projection, "a");
        assert_eq!(failures.error_row[a], None);
        assert!(failures.handled_row[a].is_some());
        assert!(!failures.subtree_error[index(&projection, "r")]);
        assert_eq!(failures.origins().count(), 0);
    }

    #[test]
    fn rows_that_say_nothing_do_not_count_as_the_error() {
        let (projection, failures) = analyze(&trace(&[
            ("r", "", 4, "start"),
            ("a", "r", 2, "2/3: at Some.Frame()"),
            ("a", "r", 2, "The message is splitted into 3 parts."),
            ("a", "r", 2, "Monitored scope end."),
        ]));
        assert_eq!(failures.error_row[index(&projection, "a")], None);

        let (projection, failures) = analyze(&trace(&[
            ("r", "", 4, "start"),
            ("a", "r", 2, "1/3: {\"message\":\"x\""),
            ("a", "r", 2, "Monitored scope end."),
        ]));
        assert_eq!(failures.error_row[index(&projection, "a")], Some(1), "the first part counts");
    }

    #[test]
    fn without_a_severity_column_nothing_failed() {
        let mut table = trace(&[("r", "", 2, "failed"), ("a", "r", 2, "failed")]);
        table.columns.remove(2);
        for row in &mut table.rows {
            row.remove(2);
        }
        let (_, failures) = analyze(&table);
        assert_eq!(failures.origins().count(), 0);
        assert!(failures.subtree_error.iter().all(|failed| !failed));
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        let ids: Vec<String> = (0..20_000).map(|index| format!("n{index}")).collect();
        let events: Vec<(&str, &str, i64, &str)> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let parent = if index == 0 { "" } else { ids[index - 1].as_str() };
                (id.as_str(), parent, if index == 19_999 { 2 } else { 4 }, "m")
            })
            .collect();
        let (_, failures) = analyze(&trace(&events));
        assert_eq!(failures.origins().count(), 1);
        assert!(failures.subtree_error[0], "the failure reaches the root");
    }
}
