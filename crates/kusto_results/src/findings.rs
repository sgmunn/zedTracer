//! What is notable about a trace, as a short list of facts that each point at the rows they come
//! from. Nothing here is guessed or comes from a model: every finding is computed from the
//! activity tree, the timestamps and the severities, and says only what those show. A duration
//! comes from logged events, and time no child explains is *not covered by traced work*, not slow.

use std::collections::HashMap;

use crate::activity::{ActivityProjection, HierarchyIssue};
use crate::failures::{Failures, row_message};
use crate::result::Table;
use crate::timeline::{Timeline, TimelineOptions};
use crate::trace_schema::TraceColumns;
use crate::trace_text::{display_names, short_marker, summarize_message};
use crate::view::severity_level;

/// Worst first, which is the order findings are listed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingSeverity {
    Error,
    Warning,
    Information,
}

/// In the order findings of one severity are listed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    FailureOrigin,
    UntracedTime,
    CriticalPath,
    Repetition,
    HandledErrors,
    SeverityMix,
    DataQuality,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: FindingSeverity,
    pub title: String,
    pub detail: Option<String>,
    /// The source rows the finding comes from, to select.
    pub rows: Vec<usize>,
    /// The activities it is about, the most important first.
    pub activities: Vec<usize>,
    /// Orders findings of one kind: the share of the trace's duration, or a count.
    pub magnitude: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FindingsOptions {
    pub timeline: TimelineOptions,
    /// Untraced time is a finding from this share of the trace's duration...
    pub untraced_share: f64,
    /// ...and at least this many ticks.
    pub untraced_minimum_ticks: i64,
    /// How many activities the critical-path finding names.
    pub critical_listed: usize,
    /// A repeat is a finding when it covers this share of the trace's duration...
    pub repeat_share: f64,
    /// ...or has this many calls and covers at least `repeat_count_share`. Many calls that take
    /// almost no time are not worth a line.
    pub repeat_count: usize,
    pub repeat_count_share: f64,
    /// Trailing parts of a process name dropped when it is shortened to show.
    pub generic_actor_suffixes: Vec<String>,
    /// A message in a title or a detail is cut to this many characters.
    pub message_characters: usize,
}

impl Default for FindingsOptions {
    fn default() -> Self {
        Self {
            timeline: TimelineOptions::default(),
            untraced_share: 0.10,
            untraced_minimum_ticks: 10 * TICKS_PER_MILLISECOND,
            critical_listed: 3,
            repeat_share: 0.05,
            repeat_count: 25,
            repeat_count_share: 0.01,
            generic_actor_suffixes: vec!["EntryPoint".into(), "Service".into()],
            message_characters: 160,
        }
    }
}

const TICKS_PER_MILLISECOND: i64 = 10_000;
const UNTRACED_LISTED: usize = 5;
/// Untraced time this large a share of the trace is a warning, not just information.
const UNTRACED_WARNING_SHARE: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct Findings {
    /// Errors first, then warnings, then information; within one severity by kind, then by
    /// magnitude.
    pub items: Vec<Finding>,
    /// The duration of the largest root, which the shares are of.
    pub trace_duration_ticks: Option<i64>,
}

struct Context<'a> {
    table: &'a Table,
    projection: &'a ActivityProjection,
    columns: &'a TraceColumns,
    options: &'a FindingsOptions,
    timeline: Timeline,
    failures: Failures,
    /// The root with the largest branch: the trace the findings are about.
    main_root: Option<usize>,
    duration: Option<i64>,
    /// Shortened actor names, by the full name.
    actor_display: HashMap<String, String>,
}

impl Findings {
    pub fn build(
        table: &Table,
        projection: &ActivityProjection,
        columns: &TraceColumns,
        options: &FindingsOptions,
    ) -> Self {
        let timeline = Timeline::build(table, projection, columns, &options.timeline);
        let failures = Failures::analyze(table, projection, columns, &timeline);
        let main_root = timeline.roots.iter().copied().reduce(|best, root| {
            let size = |activity: usize| projection.activities[activity].subtree_activity_count;
            if size(root) > size(best) { root } else { best }
        });
        let duration = main_root.and_then(|root| timeline.duration_ticks(root));
        let actor_display = actor_display_names(table, projection, columns, options);
        let context = Context {
            table,
            projection,
            columns,
            options,
            timeline,
            failures,
            main_root,
            duration,
            actor_display,
        };

        let mut items = Vec::new();
        context.failure_origins(&mut items);
        context.untraced_time(&mut items);
        context.critical_path(&mut items);
        context.repetitions(&mut items);
        context.handled_errors(&mut items);
        context.severity_mix(&mut items);
        context.data_quality(&mut items);
        items.sort_by(|left, right| {
            (left.severity, left.kind)
                .cmp(&(right.severity, right.kind))
                .then(right.magnitude.total_cmp(&left.magnitude))
        });
        Self {
            items,
            trace_duration_ticks: duration,
        }
    }
}

impl Context<'_> {
    fn first_row(&self, activity: usize) -> Option<usize> {
        self.projection.activities[activity].event_rows.first().copied()
    }

    fn marker(&self, activity: usize) -> String {
        let found = &self.projection.activities[activity];
        found
            .marker_name
            .as_deref()
            .map(short_marker)
            .unwrap_or_else(|| found.activity_id.clone())
    }

    fn actor(&self, activity: usize) -> Option<String> {
        let column = self.columns.actor?;
        let row = self.first_row(activity)?;
        let text = self.table.cell(row, column)?.display_text();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn actor_or_unknown(&self, activity: usize) -> String {
        match self.actor(activity) {
            Some(name) => self.actor_display.get(&name).cloned().unwrap_or(name),
            None => "(unknown actor)".to_string(),
        }
    }

    fn share_of_trace(&self, ticks: i64) -> Option<f64> {
        let duration = self.duration.filter(|duration| *duration > 0)?;
        Some(ticks as f64 / duration as f64)
    }

    /// The activities of the main root's branch.
    fn main_branch(&self) -> Vec<usize> {
        let mut found = Vec::new();
        let mut pending: Vec<usize> = self.main_root.into_iter().collect();
        while let Some(activity) = pending.pop() {
            found.push(activity);
            pending.extend(self.timeline.children[activity].iter().copied());
        }
        found
    }

    fn failure_origins(&self, items: &mut Vec<Finding>) {
        let mut origins: Vec<usize> = self.failures.origins().collect();
        origins.sort_by_key(|origin| {
            (
                self.timeline.start[*origin].unwrap_or(i64::MAX),
                self.first_row(*origin),
            )
        });
        let mut groups: Vec<(String, String, String, Vec<usize>)> = Vec::new();
        for origin in origins {
            let Some(row) = self.failures.error_row[origin] else {
                continue;
            };
            let message = row_message(self.table, self.columns, row)
                .map(|text| summarize_message(&text, self.options.message_characters, 1))
                .unwrap_or_default();
            let key = (self.actor_or_unknown(origin), self.marker(origin), message);
            match groups
                .iter_mut()
                .find(|(actor, marker, text, _)| (actor, marker, text) == (&key.0, &key.1, &key.2))
            {
                Some(group) => group.3.push(origin),
                None => groups.push((key.0, key.1, key.2, vec![origin])),
            }
        }
        for (actor, marker, message, origins) in groups {
            let first = origins[0];
            let mut chain = vec![first];
            let mut current = self.projection.activities[first].parent;
            while let Some(parent) = current {
                chain.push(parent);
                current = self.projection.activities[parent].parent;
            }
            let actors: std::collections::BTreeSet<String> =
                chain.iter().map(|activity| self.actor_or_unknown(*activity)).collect();
            let mut detail = message;
            if chain.len() > 1 {
                let top = self.marker(*chain.last().unwrap_or(&first));
                let passed = format!(
                    "The failure was logged again by {} calling {} in {} {} up to {top}.",
                    chain.len() - 1,
                    if chain.len() == 2 { "activity" } else { "activities" },
                    actors.len(),
                    if actors.len() == 1 { "actor" } else { "actors" },
                );
                detail = if detail.is_empty() {
                    passed
                } else if detail.ends_with(['.', '…', '!', '?']) {
                    format!("{detail} {passed}")
                } else {
                    format!("{detail}. {passed}")
                };
            }
            let count = origins.len();
            items.push(Finding {
                kind: FindingKind::FailureOrigin,
                severity: FindingSeverity::Error,
                title: format!(
                    "Failure began in {actor}: {marker}{}",
                    if count > 1 { format!(" ×{count}") } else { String::new() }
                ),
                detail: (!detail.is_empty()).then_some(detail),
                rows: origins
                    .iter()
                    .filter_map(|origin| self.failures.error_row[*origin])
                    .collect(),
                activities: chain,
                magnitude: count as f64,
            });
        }
    }

    fn untraced_time(&self, items: &mut Vec<Finding>) {
        let Some(duration) = self.duration.filter(|duration| *duration > 0) else {
            return;
        };
        let minimum = ((duration as f64 * self.options.untraced_share) as i64)
            .max(self.options.untraced_minimum_ticks);
        let mut candidates: Vec<(usize, i64)> = self
            .main_branch()
            .into_iter()
            .filter(|activity| !self.timeline.children[*activity].is_empty())
            .filter_map(|activity| Some((activity, self.timeline.untraced_ticks[activity]?)))
            .filter(|(_, ticks)| *ticks >= minimum)
            .collect();
        candidates.sort_by_key(|(activity, ticks)| (std::cmp::Reverse(*ticks), *activity));
        for (activity, ticks) in candidates.into_iter().take(UNTRACED_LISTED) {
            let own = self.timeline.duration_ticks(activity).unwrap_or(ticks);
            let share = ticks as f64 / duration as f64;
            let children = self.timeline.children[activity].len();
            let covered = own - ticks;
            let noun = if children == 1 { "child" } else { "children" };
            let covering = if covered > 0 {
                format!("Its {children} {noun} cover {}.", format_ticks(covered))
            } else {
                format!("None of it is covered by its {children} {noun}.")
            };
            items.push(Finding {
                kind: FindingKind::UntracedTime,
                severity: if share >= UNTRACED_WARNING_SHARE {
                    FindingSeverity::Warning
                } else {
                    FindingSeverity::Information
                },
                title: format!(
                    "{} of {} in {} ({}) is not covered by traced work",
                    format_ticks(ticks),
                    format_ticks(own),
                    self.marker(activity),
                    self.actor_or_unknown(activity),
                ),
                detail: Some(format!(
                    "{covering} It may be waiting on something the trace does not show, or \
                     doing work that logs nothing."
                )),
                rows: self.projection.activities[activity].event_rows.clone(),
                activities: vec![activity],
                magnitude: share,
            });
        }
    }

    fn critical_path(&self, items: &mut Vec<Finding>) {
        let Some(duration) = self.duration.filter(|duration| *duration > 0) else {
            return;
        };
        let mut contributors: Vec<(usize, i64)> = self
            .main_branch()
            .into_iter()
            .map(|activity| (activity, self.timeline.critical_ticks[activity]))
            .filter(|(_, ticks)| *ticks > 0)
            .collect();
        contributors.sort_by_key(|(activity, ticks)| (std::cmp::Reverse(*ticks), *activity));
        contributors.truncate(self.options.critical_listed);
        let Some((_, top)) = contributors.first().copied() else {
            return;
        };
        let named: Vec<String> = contributors
            .iter()
            .map(|(activity, ticks)| format!("{} {}", self.marker(*activity), format_ticks(*ticks)))
            .collect();
        items.push(Finding {
            kind: FindingKind::CriticalPath,
            severity: FindingSeverity::Information,
            title: format!("Critical path: {}", named.join(", ")),
            detail: Some(format!(
                "The critical path is the chain of work that set the trace's duration of {}. \
                 These activities have the most time of their own on it.",
                format_ticks(duration)
            )),
            rows: contributors
                .iter()
                .filter_map(|(activity, _)| self.first_row(*activity))
                .collect(),
            activities: contributors.iter().map(|(activity, _)| *activity).collect(),
            magnitude: top as f64 / duration as f64,
        });
    }

    fn repetitions(&self, items: &mut Vec<Finding>) {
        let branch: std::collections::HashSet<usize> = self.main_branch().into_iter().collect();
        let mut groups: Vec<(usize, usize, Vec<&crate::timeline::Repetition>)> = Vec::new();
        for repetition in &self.timeline.repetitions {
            if !branch.contains(&repetition.parent) {
                continue;
            }
            let count = repetition.activities.len();
            let share = self.share_of_trace(repetition.covered_ticks).unwrap_or(0.0);
            let big = count >= self.options.repeat_count && share >= self.options.repeat_count_share;
            if share < self.options.repeat_share && !big {
                continue;
            }
            // The two activities one call leaves in the callee repeat together.
            match groups
                .iter_mut()
                .find(|(parent, size, _)| (*parent, *size) == (repetition.parent, count))
            {
                Some(group) => group.2.push(repetition),
                None => groups.push((repetition.parent, count, vec![repetition])),
            }
        }
        for (parent, count, members) in groups {
            let mut markers: Vec<String> = members
                .iter()
                .map(|repetition| short_marker(&repetition.marker))
                .collect();
            if markers.len() > 2 {
                let more = markers.len() - 2;
                markers.truncate(2);
                markers.push(format!("{more} more"));
            }
            let covered = members
                .iter()
                .map(|repetition| repetition.covered_ticks)
                .max()
                .unwrap_or(0);
            let shortest = members.iter().filter_map(|r| r.shortest_ticks).min();
            let longest = members.iter().filter_map(|r| r.longest_ticks).max();
            let mut detail = format!("They cover {}", format_ticks(covered));
            if let (Some(shortest), Some(longest)) = (shortest, longest) {
                detail.push_str(&format!("; each took {}", format_range(shortest, longest)));
            }
            detail.push('.');
            let activities: Vec<usize> = members
                .iter()
                .flat_map(|repetition| repetition.activities.iter().copied())
                .collect();
            items.push(Finding {
                kind: FindingKind::Repetition,
                severity: FindingSeverity::Information,
                title: format!(
                    "{} ×{count} under {}",
                    join_names(&markers),
                    self.marker(parent)
                ),
                detail: Some(detail),
                rows: activities.iter().filter_map(|a| self.first_row(*a)).collect(),
                activities,
                magnitude: self
                    .share_of_trace(covered)
                    .unwrap_or_else(|| count as f64 / 1_000.0),
            });
        }
    }

    fn handled_errors(&self, items: &mut Vec<Finding>) {
        let handled: Vec<usize> = (0..self.failures.handled_row.len())
            .filter(|activity| self.failures.handled_row[*activity].is_some())
            .collect();
        if handled.is_empty() {
            return;
        }
        let mut by_actor: Vec<(String, usize)> = Vec::new();
        for activity in &handled {
            let actor = self.actor_or_unknown(*activity);
            match by_actor.iter_mut().find(|(name, _)| *name == actor) {
                Some(entry) => entry.1 += 1,
                None => by_actor.push((actor, 1)),
            }
        }
        let count = handled.len();
        items.push(Finding {
            kind: FindingKind::HandledErrors,
            severity: FindingSeverity::Information,
            title: format!(
                "{count} {} handled: the activity logged an error and finished normally",
                if count == 1 { "error was" } else { "errors were" }
            ),
            detail: Some(
                by_actor
                    .iter()
                    .map(|(actor, count)| format!("{actor} {count}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            rows: handled
                .iter()
                .filter_map(|activity| self.failures.handled_row[*activity])
                .collect(),
            activities: handled,
            magnitude: count as f64,
        });
    }

    fn severity_mix(&self, items: &mut Vec<Finding>) {
        let Some(column) = self.columns.severity else {
            return;
        };
        let mut by_level = [0usize; 5];
        let mut rows = Vec::new();
        for row in 0..self.table.rows.len() {
            let Some(level) = severity_level(self.table.cell(row, column)) else {
                continue;
            };
            by_level[usize::from(level) - 1] += 1;
            if level <= 3 {
                rows.push(row);
            }
        }
        if rows.is_empty() {
            return;
        }
        let names = ["critical", "error", "warning"];
        let parts: Vec<String> = by_level
            .iter()
            .zip(names)
            .filter(|(count, _)| **count > 0)
            .map(|(count, name)| format!("{count} {name}"))
            .collect();
        let count = rows.len();
        items.push(Finding {
            kind: FindingKind::SeverityMix,
            severity: FindingSeverity::Information,
            title: format!(
                "{count} {} at warning level or worse",
                if count == 1 { "row" } else { "rows" }
            ),
            detail: Some(parts.join(", ")),
            rows,
            activities: Vec::new(),
            magnitude: count as f64,
        });
    }

    fn data_quality(&self, items: &mut Vec<Finding>) {
        let mut push = |title: String, detail: Option<String>, activities: Vec<usize>| {
            let magnitude = activities.len() as f64;
            items.push(Finding {
                kind: FindingKind::DataQuality,
                severity: FindingSeverity::Information,
                title,
                detail,
                rows: activities.iter().filter_map(|a| self.first_row(*a)).collect(),
                activities,
                magnitude,
            });
        };
        let with_issue = |wanted: HierarchyIssue| -> Vec<usize> {
            (0..self.projection.activities.len())
                .filter(|activity| self.projection.activities[*activity].issue == Some(wanted))
                .collect()
        };
        let plural = |count: usize, one: &str, many: &str| if count == 1 { one.to_string() } else { many.to_string() };

        let orphans = with_issue(HierarchyIssue::Orphan);
        if !orphans.is_empty() {
            push(
                format!(
                    "{} {} caller is not in the result",
                    orphans.len(),
                    plural(orphans.len(), "activity's", "activities'")
                ),
                Some("The trace may be partial: the parent of each is missing.".to_string()),
                orphans,
            );
        }
        let conflicting = with_issue(HierarchyIssue::ConflictingParents);
        if !conflicting.is_empty() {
            push(
                format!(
                    "{} {} name more than one parent",
                    conflicting.len(),
                    plural(conflicting.len(), "activity", "activities")
                ),
                Some("No parent was chosen for them.".to_string()),
                conflicting,
            );
        }
        let cycles = with_issue(HierarchyIssue::Cycle);
        if !cycles.is_empty() {
            push(
                format!(
                    "{} parent {} loop back on themselves",
                    cycles.len(),
                    plural(cycles.len(), "chain", "chains")
                ),
                Some("Each loop was broken at the activity seen first.".to_string()),
                cycles,
            );
        }
        if !self.timeline.outside_parent.is_empty() {
            push(
                format!(
                    "{} {} outside the time range of {} parent",
                    self.timeline.outside_parent.len(),
                    plural(self.timeline.outside_parent.len(), "activity runs", "activities run"),
                    plural(self.timeline.outside_parent.len(), "its", "their")
                ),
                Some(
                    "The clocks of different processes can disagree. The parent's time was \
                     widened to cover them."
                        .to_string(),
                ),
                self.timeline.outside_parent.clone(),
            );
        }
        if !self.timeline.without_bounds.is_empty() && self.columns.timestamp.is_some() {
            push(
                format!(
                    "{} {} no readable timestamp",
                    self.timeline.without_bounds.len(),
                    plural(self.timeline.without_bounds.len(), "activity has", "activities have")
                ),
                Some("They have no duration and are left out of the time findings.".to_string()),
                self.timeline.without_bounds.clone(),
            );
        }
        if self.columns.severity.is_none() {
            push(
                "No severity column was found".to_string(),
                Some("Errors and warnings cannot be told apart from other rows.".to_string()),
                Vec::new(),
            );
        }
        if self.columns.timestamp.is_none() {
            push(
                "No timestamp column was found".to_string(),
                Some("Durations and the time findings are not available.".to_string()),
                Vec::new(),
            );
        }
    }
}

/// `a`, `a and b`, or `a, b and c`.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn format_range(shortest: i64, longest: i64) -> String {
    let (low, high) = (format_ticks(shortest), format_ticks(longest));
    if low == high { low } else { format!("{low} to {high}") }
}

/// The shortened name of each actor in the table, by its full name.
fn actor_display_names(
    table: &Table,
    projection: &ActivityProjection,
    columns: &TraceColumns,
    options: &FindingsOptions,
) -> HashMap<String, String> {
    let Some(column) = columns.actor else {
        return HashMap::new();
    };
    let mut names: Vec<String> = Vec::new();
    for activity in &projection.activities {
        let Some(row) = activity.event_rows.first() else {
            continue;
        };
        let Some(cell) = table.cell(*row, column) else {
            continue;
        };
        let name = cell.display_text().trim().to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    let displays = display_names(&names, &options.generic_actor_suffixes);
    names.into_iter().zip(displays).collect()
}

/// A duration the way a person reads it: seconds from one second up, else milliseconds.
pub fn format_ticks(ticks: i64) -> String {
    let milliseconds = ticks as f64 / TICKS_PER_MILLISECOND as f64;
    if milliseconds >= 1000.0 {
        format!("{:.1} s", milliseconds / 1000.0)
    } else if milliseconds >= 1.0 {
        format!("{} ms", milliseconds.round() as i64)
    } else {
        "under 1 ms".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::build_projection_with;
    use crate::result::{Cell, Column};

    struct Event {
        activity: &'static str,
        parent: &'static str,
        actor: &'static str,
        marker: &'static str,
        millis: i64,
        level: i64,
        message: &'static str,
    }

    fn event(
        activity: &'static str,
        parent: &'static str,
        actor: &'static str,
        marker: &'static str,
        millis: i64,
    ) -> Event {
        Event {
            activity,
            parent,
            actor,
            marker,
            millis,
            level: 4,
            message: "ok",
        }
    }

    fn failing(mut event: Event, message: &'static str) -> Event {
        event.level = 2;
        event.message = message;
        event
    }

    fn span(
        events: &mut Vec<Event>,
        activity: &'static str,
        parent: &'static str,
        actor: &'static str,
        marker: &'static str,
        start: i64,
        end: i64,
    ) {
        events.push(event(activity, parent, actor, marker, start));
        events.push(event(activity, parent, actor, marker, end));
    }

    fn trace(events: Vec<Event>) -> Table {
        Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("ProcessName", "string"),
                Column::new("MarkerName", "string"),
                Column::new("TIMESTAMP", "datetime"),
                Column::new("Level", "long"),
                Column::new("MessageText", "string"),
            ],
            rows: events
                .into_iter()
                .map(|event| {
                    let text = |value: &str| {
                        if value.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Text(value.into())
                        }
                    };
                    vec![
                        text(event.activity),
                        text(event.parent),
                        text(event.actor),
                        text(event.marker),
                        Cell::Text(format!(
                            "2026-01-01T00:{:02}:{:02}.{:07}Z",
                            event.millis / 60_000,
                            (event.millis / 1000) % 60,
                            (event.millis % 1000) * 10_000
                        )),
                        Cell::Int(event.level),
                        Cell::Text(event.message.into()),
                    ]
                })
                .collect(),
        }
    }

    fn findings(table: &Table) -> Findings {
        let columns = TraceColumns::detect(table);
        let projection = build_projection_with(table, &columns).expect("a projection");
        Findings::build(table, &projection, &columns, &FindingsOptions::default())
    }

    fn of_kind(findings: &Findings, kind: FindingKind) -> Vec<&Finding> {
        findings.items.iter().filter(|finding| finding.kind == kind).collect()
    }

    #[test]
    fn a_failure_is_told_where_it_began_with_the_path_up() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "Svc", "Job.Run", 0, 100);
        events.push(failing(event("a", "r", "Svc", "Ask", 10), "the call failed"));
        events.push(failing(event("b", "a", "Db", "Db.Query", 20), "no such row"));
        let result = findings(&trace(events));
        let origins = of_kind(&result, FindingKind::FailureOrigin);
        assert_eq!(origins.len(), 1, "the caller's copy is not another failure");
        let origin = origins[0];
        assert_eq!(origin.severity, FindingSeverity::Error);
        assert_eq!(origin.title, "Failure began in Db: Db.Query");
        let detail = origin.detail.as_deref().expect("a detail");
        assert!(detail.starts_with("no such row."), "{detail}");
        assert!(detail.contains("logged again by 2 calling activities in 2 actors up to Job.Run"), "{detail}");
        assert_eq!(origin.rows, vec![3], "the row of the origin's error");
        assert_eq!(origin.activities.len(), 3);
    }

    #[test]
    fn the_same_failure_in_many_places_is_one_finding_with_a_count() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 100);
        for (index, id) in ["k0", "k1", "k2"].into_iter().enumerate() {
            events.push(failing(event(id, "r", "B", "Entry", 10 + index as i64), "timeout"));
        }
        let result = findings(&trace(events));
        let origins = of_kind(&result, FindingKind::FailureOrigin);
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].title, "Failure began in B: Entry ×3");
        assert_eq!(origins[0].rows.len(), 3);
    }

    #[test]
    fn time_no_child_explains_is_a_finding_only_when_it_is_large() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 1000);
        span(&mut events, "gap", "r", "A", "Waits", 100, 900);
        span(&mut events, "k", "gap", "A", "Short", 120, 140);
        span(&mut events, "tidy", "r", "A", "Tidy", 950, 960);
        span(&mut events, "t1", "tidy", "A", "Inner", 950, 959);
        let result = findings(&trace(events));
        let untraced = of_kind(&result, FindingKind::UntracedTime);
        let titles: Vec<&str> = untraced.iter().map(|finding| finding.title.as_str()).collect();
        assert!(
            titles.iter().any(|title| title.starts_with("780 ms of 800 ms in Waits (A) is not covered by traced work")),
            "{titles:?}"
        );
        assert!(
            !titles.iter().any(|title| title.contains("Tidy")),
            "1 ms is under the minimum: {titles:?}"
        );
        assert_eq!(untraced[0].severity, FindingSeverity::Warning, "78% of the trace is above half");
        let detail = untraced[0].detail.as_deref().expect("a detail");
        assert!(detail.contains("may be waiting on something the trace does not show"), "{detail}");
    }

    #[test]
    fn the_critical_path_names_the_activities_with_the_most_time_of_their_own() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 1000);
        span(&mut events, "x", "r", "A", "Slow", 0, 700);
        span(&mut events, "y", "r", "A", "Quick", 700, 800);
        let result = findings(&trace(events));
        let critical = of_kind(&result, FindingKind::CriticalPath);
        assert_eq!(critical.len(), 1);
        assert!(critical[0].title.starts_with("Critical path: Slow 700 ms, Run 200 ms, Quick 100 ms"), "{}", critical[0].title);
        assert_eq!(critical[0].activities.len(), 3);
    }

    fn leaked(id: String) -> &'static str {
        Box::leak(id.into_boxed_str())
    }

    #[test]
    fn a_repeat_is_a_finding_when_it_covers_time_or_is_big_and_not_negligible() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 10_000);
        for index in 0..26 {
            let start = 100 + index * 20;
            span(&mut events, leaked(format!("t{index}")), "r", "A", "Frequent", start, start + 5);
        }
        for index in 0..30 {
            let start = 700 + index * 10;
            span(&mut events, leaked(format!("d{index}")), "r", "A", "Dust", start, start + 1);
        }
        for index in 0..6 {
            let start = 1000 + index * 500;
            span(&mut events, leaked(format!("l{index}")), "r", "A", "Long", start, start + 400);
        }
        for index in 0..6 {
            let start = 5000 + index * 10;
            span(&mut events, leaked(format!("b{index}")), "r", "A", "Brief", start, start + 1);
        }
        let result = findings(&trace(events));
        let titles: Vec<String> = of_kind(&result, FindingKind::Repetition)
            .iter()
            .map(|finding| finding.title.clone())
            .collect();
        assert!(titles.contains(&"Frequent ×26 under Run".to_string()), "26 calls and 1.3%: {titles:?}");
        assert!(titles.contains(&"Long ×6 under Run".to_string()), "covers 24%: {titles:?}");
        assert!(!titles.iter().any(|title| title.starts_with("Dust")), "30 calls but 0.3%: {titles:?}");
        assert!(!titles.iter().any(|title| title.starts_with("Brief")), "6 brief calls: {titles:?}");
        let long = of_kind(&result, FindingKind::Repetition)
            .into_iter()
            .find(|finding| finding.title.starts_with("Long"))
            .expect("the long repeat");
        assert_eq!(long.detail.as_deref(), Some("They cover 2.4 s; each took 400 ms."));
    }

    #[test]
    fn repeats_of_one_call_are_one_finding_and_long_lists_are_cut() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 10_000);
        for (index, marker) in ["One", "Two", "Three", "Four"].into_iter().enumerate() {
            for call in 0..5 {
                let start = 100 + call * 1500 + index as i64 * 100;
                span(&mut events, leaked(format!("{marker}{call}")), "r", "A", marker, start, start + 120);
            }
        }
        let result = findings(&trace(events));
        let repeats = of_kind(&result, FindingKind::Repetition);
        assert_eq!(repeats.len(), 1);
        assert_eq!(repeats[0].title, "One, Two and 2 more ×5 under Run");
    }

    #[test]
    fn actors_are_shown_with_their_short_names() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "Microsoft.Foo.Alpha.Service", "Run", 0, 100);
        events.push(failing(event("a", "r", "Microsoft.Foo.Beta.Service", "Db.Query", 10), "broke"));
        let result = findings(&trace(events));
        assert_eq!(of_kind(&result, FindingKind::FailureOrigin)[0].title, "Failure began in Beta: Db.Query");
    }

    #[test]
    fn a_leaf_is_never_untraced_time_and_the_wording_fits_the_children() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 1000);
        span(&mut events, "leaf", "r", "A", "Slow", 0, 900);
        span(&mut events, "only", "r", "A", "Waits", 900, 1000);
        span(&mut events, "kid", "only", "A", "Kid", 900, 900);
        let result = findings(&trace(events));
        let untraced = of_kind(&result, FindingKind::UntracedTime);
        assert!(!untraced.iter().any(|finding| finding.title.contains("Slow")), "a leaf has no children to explain it");
        let waits = untraced
            .iter()
            .find(|finding| finding.title.contains("Waits"))
            .expect("Waits has one child and 100 ms to itself");
        let detail = waits.detail.as_deref().expect("a detail");
        assert!(detail.starts_with("None of it is covered by its 1 child."), "{detail}");
    }

    #[test]
    fn errors_that_were_recovered_from_are_counted_by_actor() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 100);
        events.push(failing(event("a", "r", "B", "Try", 10), "retrying"));
        events.push(event("a", "r", "B", "Try", 20));
        let result = findings(&trace(events));
        let handled = of_kind(&result, FindingKind::HandledErrors);
        assert_eq!(handled.len(), 1);
        assert_eq!(handled[0].title, "1 error was handled: the activity logged an error and finished normally");
        assert_eq!(handled[0].detail.as_deref(), Some("B 1"));
        assert!(of_kind(&result, FindingKind::FailureOrigin).is_empty());
    }

    #[test]
    fn the_severity_mix_links_to_the_rows_at_warning_level_or_worse() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 100);
        let mut warning = event("w", "r", "A", "Warn", 10);
        warning.level = 3;
        events.push(warning);
        let result = findings(&trace(events));
        let mix = of_kind(&result, FindingKind::SeverityMix);
        assert_eq!(mix.len(), 1);
        assert_eq!(mix[0].title, "1 row at warning level or worse");
        assert_eq!(mix[0].detail.as_deref(), Some("1 warning"));
        assert_eq!(mix[0].rows, vec![2]);
    }

    #[test]
    fn a_partial_trace_and_odd_clocks_are_data_quality_findings() {
        let mut events = Vec::new();
        span(&mut events, "r", "missing-parent", "A", "Run", 100, 200);
        span(&mut events, "early", "r", "B", "Early", 50, 150);
        let result = findings(&trace(events));
        let quality: Vec<&str> = of_kind(&result, FindingKind::DataQuality)
            .iter()
            .map(|finding| finding.title.as_str())
            .collect();
        assert!(quality.contains(&"1 activity's caller is not in the result"), "{quality:?}");
        assert!(quality.contains(&"1 activity runs outside the time range of its parent"), "{quality:?}");
    }

    #[test]
    fn a_trace_without_severity_or_timestamps_says_what_it_lacks() {
        let mut table = trace(vec![event("r", "", "A", "Run", 0), event("a", "r", "A", "Ask", 1)]);
        table.columns.remove(5);
        for row in &mut table.rows {
            row.remove(5);
        }
        let result = findings(&table);
        assert!(result.items.iter().any(|finding| finding.title == "No severity column was found"));
        assert!(result.trace_duration_ticks.is_some());

        let mut table = trace(vec![event("r", "", "A", "Run", 0), event("a", "r", "A", "Ask", 1)]);
        table.columns.remove(4);
        for row in &mut table.rows {
            row.remove(4);
        }
        let result = findings(&table);
        assert!(result.items.iter().any(|finding| finding.title == "No timestamp column was found"));
        assert_eq!(result.trace_duration_ticks, None);
        assert!(of_kind(&result, FindingKind::UntracedTime).is_empty());
    }

    #[test]
    fn findings_are_listed_errors_first_then_by_kind_and_size() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "A", "Run", 0, 1000);
        span(&mut events, "gap", "r", "A", "Waits", 100, 900);
        events.push(failing(event("f", "r", "B", "Entry", 950), "broke"));
        let mut warning = event("w", "r", "A", "Warn", 960);
        warning.level = 3;
        events.push(warning);
        let result = findings(&trace(events));
        let severities: Vec<FindingSeverity> = result.items.iter().map(|finding| finding.severity).collect();
        let mut sorted = severities.clone();
        sorted.sort();
        assert_eq!(severities, sorted, "errors, then warnings, then information");
        assert_eq!(result.items[0].kind, FindingKind::FailureOrigin);
    }

    #[test]
    fn durations_read_as_a_person_would() {
        assert_eq!(format_ticks(4_202 * TICKS_PER_MILLISECOND), "4.2 s");
        assert_eq!(format_ticks(999 * TICKS_PER_MILLISECOND), "999 ms");
        assert_eq!(format_ticks(5_000), "under 1 ms");
    }

    /// Needs the real traces in `fork-docs/samples`, which are not committed.
    /// Run with `cargo test -p kusto_results --lib findings_of_real_traces -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn findings_of_real_traces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fork-docs/samples");
        for name in ["sample1.ktt", "sample2.ktt"] {
            let Ok(text) = std::fs::read_to_string(root.join(name)) else {
                eprintln!("{name} is not here, skipped");
                continue;
            };
            let result = crate::result::ResultSet::from_json(&text).expect("a result file");
            let table = &result.tables[0];
            let started = std::time::Instant::now();
            let found = findings(table);
            eprintln!("{name}: {} findings in {:?}", found.items.len(), started.elapsed());
            for finding in &found.items {
                eprintln!(
                    "  [{:?}] {}  ({} rows)",
                    finding.severity,
                    finding.title,
                    finding.rows.len()
                );
                if let Some(detail) = &finding.detail {
                    eprintln!("        {detail}");
                }
            }
        }
    }
}
