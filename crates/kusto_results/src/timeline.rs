//! The facts about time in a structured trace: when each activity ran, how much of that time
//! its children explain, where the time went on the way to the end, and what repeats.
//!
//! Everything is computed from the activity tree and the timestamps of the events. Times are
//! only as good as what was logged: an activity runs from its first event to its last, so one
//! with a single event has no duration, and one with none has no bounds at all. An activity
//! that has bounds is widened to cover its descendants, because a scope is still running while
//! the work it started is: a parent that logged only at its start still lasts as long as its
//! children.

use std::collections::HashMap;

use crate::activity::ActivityProjection;
use crate::result::Table;
use crate::trace_schema::{TraceColumns, matches_pattern};
use crate::typed::parse_datetime_ticks;

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineOptions {
    /// Messages that only say something started or ended and carry no content. A `*` at either
    /// end matches any text.
    pub structural_messages: Vec<String>,
    /// How many children of one parent must share a marker to count as a repeat.
    pub minimum_repeat: usize,
}

impl Default for TimelineOptions {
    fn default() -> Self {
        Self {
            structural_messages: vec![
                "Monitored scope start*".into(),
                "Monitored scope end*".into(),
            ],
            minimum_repeat: 5,
        }
    }
}

/// Children of one parent that share a marker.
#[derive(Debug, Clone, PartialEq)]
pub struct Repetition {
    pub parent: usize,
    pub marker: String,
    pub activities: Vec<usize>,
    /// The time the repeats cover, overlaps counted once, in ticks.
    pub covered_ticks: i64,
    pub shortest_ticks: Option<i64>,
    pub longest_ticks: Option<i64>,
    pub first_start: Option<i64>,
    pub last_start: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    /// Earliest and latest event time of each activity, in 100 ns ticks, widened to cover its
    /// descendants. `None` when none of its own events has a readable timestamp.
    pub start: Vec<Option<i64>>,
    pub end: Vec<Option<i64>>,
    pub children: Vec<Vec<usize>>,
    pub roots: Vec<usize>,
    /// Time an activity's children do not explain, in ticks: its duration minus the union of its
    /// children's intervals. `None` without bounds.
    pub untraced_ticks: Vec<Option<i64>>,
    /// Ticks of an activity's own time that lie on the critical path of its root. For a root with
    /// bounds the values of its branch add up to its duration.
    pub critical_ticks: Vec<i64>,
    pub repetitions: Vec<Repetition>,
    /// Activities whose own events start before, or end after, their parent's own events. The
    /// parent's bounds were widened to cover them.
    pub outside_parent: Vec<usize>,
    /// Activities none of whose events has a readable timestamp.
    pub without_bounds: Vec<usize>,
    /// For each source row, whether it only says something started or ended.
    pub structural_rows: Vec<bool>,
    pub trace_start: Option<i64>,
    pub trace_end: Option<i64>,
}

impl Timeline {
    pub fn build(
        table: &Table,
        projection: &ActivityProjection,
        columns: &TraceColumns,
        options: &TimelineOptions,
    ) -> Self {
        let count = projection.activities.len();
        let mut start = vec![None; count];
        let mut end = vec![None; count];
        if let Some(column) = columns.timestamp {
            for (index, activity) in projection.activities.iter().enumerate() {
                for row in &activity.event_rows {
                    let Some(cell) = table.cell(*row, column) else {
                        continue;
                    };
                    let Some(ticks) = parse_datetime_ticks(&cell.display_text()) else {
                        continue;
                    };
                    start[index] = Some(start[index].map_or(ticks, |low: i64| low.min(ticks)));
                    end[index] = Some(end[index].map_or(ticks, |high: i64| high.max(ticks)));
                }
            }
        }

        let mut children: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut roots = Vec::new();
        for (index, activity) in projection.activities.iter().enumerate() {
            match activity.parent {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }

        let mut timeline = Self {
            trace_start: None,
            trace_end: None,
            start,
            end,
            children,
            roots,
            untraced_ticks: vec![None; count],
            critical_ticks: vec![0; count],
            repetitions: Vec::new(),
            outside_parent: Vec::new(),
            without_bounds: Vec::new(),
            structural_rows: structural_rows(table, columns, &options.structural_messages),
        };
        timeline.find_outside_parent(projection);
        timeline.widen_to_descendants(projection);
        timeline.trace_start = timeline.roots.iter().filter_map(|root| timeline.start[*root]).min();
        timeline.trace_end = timeline.roots.iter().filter_map(|root| timeline.end[*root]).max();
        timeline.find_untraced_time(projection);
        timeline.find_critical_path();
        timeline.find_repetitions(projection, options.minimum_repeat);
        timeline
    }

    /// An activity's duration in ticks, when it has bounds.
    pub fn duration_ticks(&self, activity: usize) -> Option<i64> {
        Some(self.end.get(activity).copied()?? - self.start.get(activity).copied()??)
    }

    /// The part of a child's interval that lies inside its parent's, when there is any of it.
    fn clipped(&self, child: usize, window_start: i64, window_end: i64) -> Option<(i64, i64)> {
        let start = self.start[child]?.max(window_start);
        let end = self.end[child]?.min(window_end);
        (end > start).then_some((start, end))
    }

    fn find_outside_parent(&mut self, projection: &ActivityProjection) {
        for index in 0..projection.activities.len() {
            let (Some(start), Some(end)) = (self.start[index], self.end[index]) else {
                continue;
            };
            for child in &self.children[index] {
                if let (Some(child_start), Some(child_end)) = (self.start[*child], self.end[*child])
                {
                    if child_start < start || child_end > end {
                        self.outside_parent.push(*child);
                    }
                }
            }
        }
        self.outside_parent.sort_unstable();
        self.outside_parent.dedup();
    }

    /// Bottom up, so a grandchild's time reaches the grandparent. An activity with no bounds of
    /// its own stays without them rather than being given its children's.
    fn widen_to_descendants(&mut self, projection: &ActivityProjection) {
        let mut preorder = Vec::with_capacity(projection.activities.len());
        let mut pending: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(activity) = pending.pop() {
            preorder.push(activity);
            pending.extend(self.children[activity].iter().rev().copied());
        }
        for activity in preorder.into_iter().rev() {
            let Some(parent) = projection.activities[activity].parent else {
                continue;
            };
            let (Some(start), Some(end)) = (self.start[activity], self.end[activity]) else {
                continue;
            };
            if let Some(parent_start) = self.start[parent] {
                self.start[parent] = Some(parent_start.min(start));
            }
            if let Some(parent_end) = self.end[parent] {
                self.end[parent] = Some(parent_end.max(end));
            }
        }
    }

    fn find_untraced_time(&mut self, projection: &ActivityProjection) {
        for index in 0..projection.activities.len() {
            let (Some(start), Some(end)) = (self.start[index], self.end[index]) else {
                self.without_bounds.push(index);
                continue;
            };
            let mut intervals: Vec<(i64, i64)> = self.children[index]
                .iter()
                .filter_map(|child| self.clipped(*child, start, end))
                .collect();
            self.untraced_ticks[index] = Some((end - start) - union_length(&mut intervals));
        }
    }

    /// Walks back from the end of each root. The child that finishes last is on the path, and the
    /// gap between it and the cursor is the parent's own time; the walk then continues from the
    /// child's start and skips whatever overlapped it. Each child on the path is walked the same
    /// way inside its own interval, so every tick of a root belongs to exactly one activity.
    fn find_critical_path(&mut self) {
        let mut pending: Vec<(usize, i64, i64)> = Vec::new();
        for root in self.roots.clone() {
            if let (Some(start), Some(end)) = (self.start[root], self.end[root]) {
                pending.push((root, start, end));
            }
        }
        while let Some((activity, window_start, window_end)) = pending.pop() {
            let mut on_path: Vec<(usize, i64, i64)> = self.children[activity]
                .iter()
                .filter_map(|child| {
                    let (start, end) = self.clipped(*child, window_start, window_end)?;
                    Some((*child, start, end))
                })
                .collect();
            on_path.sort_by(|left, right| right.2.cmp(&left.2).then(right.1.cmp(&left.1)));

            let mut cursor = window_end;
            let mut own = 0;
            for (child, start, end) in on_path {
                if end > cursor {
                    continue;
                }
                own += cursor - end;
                pending.push((child, start, end));
                cursor = start;
            }
            own += cursor - window_start;
            self.critical_ticks[activity] += own;
        }
    }

    fn find_repetitions(&mut self, projection: &ActivityProjection, minimum: usize) {
        for (parent, kids) in self.children.iter().enumerate() {
            if kids.len() < minimum.max(2) {
                continue;
            }
            let mut order: Vec<&str> = Vec::new();
            let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
            for kid in kids {
                let Some(marker) = projection.activities[*kid].marker_name.as_deref() else {
                    continue;
                };
                groups
                    .entry(marker)
                    .or_insert_with(|| {
                        order.push(marker);
                        Vec::new()
                    })
                    .push(*kid);
            }
            for marker in order {
                let Some(members) = groups.remove(marker) else {
                    continue;
                };
                if members.len() < minimum {
                    continue;
                }
                let durations: Vec<i64> = members
                    .iter()
                    .filter_map(|member| self.duration_ticks(*member))
                    .collect();
                let mut intervals: Vec<(i64, i64)> = members
                    .iter()
                    .filter_map(|member| Some((self.start[*member]?, self.end[*member]?)))
                    .collect();
                let starts = members.iter().filter_map(|member| self.start[*member]);
                self.repetitions.push(Repetition {
                    parent,
                    marker: marker.to_string(),
                    covered_ticks: union_length(&mut intervals),
                    shortest_ticks: durations.iter().copied().min(),
                    longest_ticks: durations.iter().copied().max(),
                    first_start: starts.clone().min(),
                    last_start: starts.max(),
                    activities: members,
                });
            }
        }
    }
}

/// The total length covered by a set of intervals, overlaps counted once.
fn union_length(intervals: &mut [(i64, i64)]) -> i64 {
    intervals.sort_unstable();
    let mut total = 0;
    let mut current: Option<(i64, i64)> = None;
    for (start, end) in intervals.iter().copied() {
        match current {
            Some((current_start, current_end)) if start <= current_end => {
                current = Some((current_start, current_end.max(end)));
            }
            Some((current_start, current_end)) => {
                total += current_end - current_start;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((start, end)) = current {
        total += end - start;
    }
    total
}

/// For each source row, whether its message only says something started or ended, by the
/// patterns of `structural_messages`. All `false` when the table has no message column.
pub fn structural_rows(table: &Table, columns: &TraceColumns, patterns: &[String]) -> Vec<bool> {
    let Some(message) = columns.message else {
        return vec![false; table.rows.len()];
    };
    (0..table.rows.len())
        .map(|row| {
            table.cell(row, message).is_some_and(|cell| {
                let text = cell.display_text();
                let text = text.trim();
                patterns.iter().any(|pattern| matches_pattern(pattern, text))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::build_projection_with;
    use crate::result::{Cell, Column};
    use crate::typed::TICKS_PER_SECOND;
    use pretty_assertions::assert_eq;

    const MILLISECOND: i64 = TICKS_PER_SECOND / 1000;

    struct Event {
        activity: &'static str,
        parent: &'static str,
        marker: &'static str,
        millis: i64,
        message: &'static str,
    }

    fn event(
        activity: &'static str,
        parent: &'static str,
        marker: &'static str,
        millis: i64,
    ) -> Event {
        Event {
            activity,
            parent,
            marker,
            millis,
            message: "ok",
        }
    }

    fn trace(events: Vec<Event>) -> Table {
        Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("MarkerName", "string"),
                Column::new("TIMESTAMP", "datetime"),
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
                        text(event.marker),
                        Cell::Text(format!(
                            "2026-01-01T00:{:02}:{:02}.{:07}Z",
                            event.millis / 60_000,
                            (event.millis / 1000) % 60,
                            (event.millis % 1000) * 10_000
                        )),
                        text(event.message),
                    ]
                })
                .collect(),
        }
    }

    fn build(table: &Table) -> (ActivityProjection, Timeline) {
        let columns = TraceColumns::detect(table);
        let projection = build_projection_with(table, &columns).expect("a projection");
        let timeline = Timeline::build(table, &projection, &columns, &TimelineOptions::default());
        (projection, timeline)
    }

    fn index(projection: &ActivityProjection, id: &str) -> usize {
        projection
            .activities
            .iter()
            .position(|activity| activity.activity_id == id)
            .expect("the activity exists")
    }

    /// An activity that runs from `start` to `end` milliseconds.
    fn span(
        events: &mut Vec<Event>,
        activity: &'static str,
        parent: &'static str,
        marker: &'static str,
        start: i64,
        end: i64,
    ) {
        events.push(event(activity, parent, marker, start));
        events.push(event(activity, parent, marker, end));
    }

    #[test]
    fn an_activity_runs_from_its_first_event_to_its_last() {
        let (projection, timeline) = build(&trace(vec![
            event("a", "", "A", 10),
            event("a", "", "A", 40),
            event("a", "", "A", 25),
            event("b", "", "B", 50),
        ]));
        let a = index(&projection, "a");
        let b = index(&projection, "b");
        assert_eq!(timeline.duration_ticks(a), Some(30 * MILLISECOND));
        assert_eq!(timeline.duration_ticks(b), Some(0), "one event has no duration");
        assert_eq!(timeline.trace_start, timeline.start[a]);
        assert_eq!(timeline.trace_end, timeline.end[b]);
    }

    #[test]
    fn unreadable_timestamps_give_no_bounds() {
        let mut table = trace(vec![event("a", "", "A", 10), event("a", "", "A", 20)]);
        for row in &mut table.rows {
            row[3] = Cell::Text("not a time".into());
        }
        let (projection, timeline) = build(&table);
        assert_eq!(timeline.start[0], None);
        assert_eq!(timeline.duration_ticks(0), None);
        assert_eq!(timeline.untraced_ticks[0], None);
        assert_eq!(timeline.without_bounds, vec![index(&projection, "a")]);
        assert_eq!(timeline.trace_start, None);
    }

    #[test]
    fn a_grandchild_widens_its_grandparent_but_an_activity_with_no_bounds_stays_without() {
        let mut events = Vec::new();
        span(&mut events, "g", "", "G", 0, 10);
        events.push(event("silent", "g", "S", 5));
        span(&mut events, "leaf", "silent", "L", 20, 90);
        let mut table = trace(events);
        for row in &mut table.rows {
            if row[0] == Cell::Text("silent".into()) {
                row[3] = Cell::Text("not a time".into());
            }
        }
        let (projection, timeline) = build(&table);
        let g = index(&projection, "g");
        assert_eq!(timeline.duration_ticks(g), Some(10 * MILLISECOND), "silent has no bounds to pass up");
        let silent = index(&projection, "silent");
        assert_eq!(timeline.start[silent], None);
        assert_eq!(timeline.without_bounds, vec![silent]);
    }

    #[test]
    fn untraced_time_is_what_the_children_do_not_cover() {
        let mut events = Vec::new();
        span(&mut events, "p", "", "P", 0, 100);
        span(&mut events, "a", "p", "A", 10, 30);
        span(&mut events, "b", "p", "B", 20, 50);
        span(&mut events, "c", "p", "C", 80, 90);
        let (projection, timeline) = build(&trace(events));
        let p = index(&projection, "p");
        assert_eq!(timeline.untraced_ticks[p], Some(50 * MILLISECOND), "[10,50] and [80,90] are covered");
        assert_eq!(timeline.untraced_ticks[index(&projection, "a")], Some(20 * MILLISECOND));
    }

    #[test]
    fn a_parent_is_widened_to_cover_its_children_and_they_are_counted() {
        let mut events = Vec::new();
        span(&mut events, "p", "", "P", 100, 200);
        span(&mut events, "early", "p", "E", 50, 120);
        span(&mut events, "outside", "p", "O", 300, 400);
        let (projection, timeline) = build(&trace(events));
        let p = index(&projection, "p");
        assert_eq!(timeline.start[p], timeline.start[index(&projection, "early")], "the earliest descendant");
        assert_eq!(timeline.end[p], timeline.end[index(&projection, "outside")], "the latest descendant");
        assert_eq!(timeline.duration_ticks(p), Some(350 * MILLISECOND));
        assert_eq!(timeline.untraced_ticks[p], Some(180 * MILLISECOND), "[50,120] and [300,400] are covered");
        assert_eq!(timeline.trace_end, timeline.end[p]);
        let mut expected = vec![index(&projection, "early"), index(&projection, "outside")];
        expected.sort_unstable();
        assert_eq!(timeline.outside_parent, expected);
    }

    #[test]
    fn the_critical_path_walks_back_from_the_end_and_skips_parallel_work() {
        let mut events = Vec::new();
        span(&mut events, "p", "", "P", 0, 100);
        span(&mut events, "a", "p", "A", 0, 40);
        span(&mut events, "b", "p", "B", 10, 60);
        span(&mut events, "c", "p", "C", 70, 90);
        let (projection, timeline) = build(&trace(events));
        let ticks = |id: &str| timeline.critical_ticks[index(&projection, id)];
        assert_eq!(ticks("c"), 20 * MILLISECOND);
        assert_eq!(ticks("b"), 50 * MILLISECOND);
        assert_eq!(ticks("a"), 0, "a overlaps b, which finished later");
        assert_eq!(ticks("p"), 30 * MILLISECOND, "10 after c, 10 between b and c, 10 before b");
    }

    #[test]
    fn the_critical_path_finds_the_long_gap_that_the_last_ending_child_hides() {
        let mut events = Vec::new();
        span(&mut events, "root", "", "Root", 0, 4720);
        span(&mut events, "loop", "root", "Loop", 45, 4539);
        for step in 0..5 {
            span(&mut events, ["l0", "l1", "l2", "l3", "l4"][step], "loop", "Call", 45 + step as i64 * 900, 45 + step as i64 * 900 + 20);
        }
        span(&mut events, "last", "root", "Last", 4644, 4720);
        let (projection, timeline) = build(&trace(events));
        let loop_activity = index(&projection, "loop");
        assert!(
            timeline.critical_ticks[loop_activity] > 4000 * MILLISECOND,
            "the loop's own time is on the path: {}",
            timeline.critical_ticks[loop_activity] / MILLISECOND
        );
        assert_eq!(timeline.critical_ticks[index(&projection, "last")], 76 * MILLISECOND);
    }

    #[test]
    fn every_tick_of_a_root_belongs_to_one_activity_on_its_path() {
        let mut events = Vec::new();
        span(&mut events, "r", "", "R", 0, 1000);
        span(&mut events, "a", "r", "A", 50, 600);
        span(&mut events, "b", "r", "B", 400, 900);
        span(&mut events, "c", "r", "C", 950, 1400);
        span(&mut events, "a1", "a", "A1", 60, 300);
        span(&mut events, "a2", "a", "A2", 200, 580);
        span(&mut events, "b1", "b", "B1", 410, 700);
        span(&mut events, "b2", "b", "B2", 680, 950);
        span(&mut events, "c1", "c", "C1", 960, 1300);
        let (projection, timeline) = build(&trace(events));
        let total: i64 = timeline.critical_ticks.iter().sum();
        let root = index(&projection, "r");
        assert_eq!(Some(total), timeline.duration_ticks(root));
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        let ids: Vec<&'static str> = (0..20_000)
            .map(|index| &*Box::leak(format!("n{index}").into_boxed_str()))
            .collect();
        let mut events = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let parent = if index == 0 { "" } else { ids[index - 1] };
            span(&mut events, id, parent, "Hop", index as i64, 100_000 - index as i64);
        }
        let (_, timeline) = build(&trace(events));
        assert_eq!(timeline.critical_ticks.iter().sum::<i64>(), 100_000 * MILLISECOND);
    }

    #[test]
    fn children_that_share_a_marker_form_a_repeat_only_from_the_minimum() {
        let mut events = Vec::new();
        span(&mut events, "p", "", "P", 0, 1000);
        for (index, id) in ["r0", "r1", "r2", "r3", "r4", "r5"].into_iter().enumerate() {
            span(&mut events, id, "p", "Poll", index as i64 * 100, index as i64 * 100 + 10 + index as i64);
        }
        for (index, id) in ["o0", "o1", "o2", "o3"].into_iter().enumerate() {
            span(&mut events, id, "p", "Other", 700 + index as i64 * 10, 705 + index as i64 * 10);
        }
        let (projection, timeline) = build(&trace(events));
        assert_eq!(timeline.repetitions.len(), 1, "four of Other is under the minimum");
        let repeat = &timeline.repetitions[0];
        assert_eq!(repeat.marker, "Poll");
        assert_eq!(repeat.parent, index(&projection, "p"));
        assert_eq!(repeat.activities.len(), 6);
        assert_eq!(repeat.shortest_ticks, Some(10 * MILLISECOND));
        assert_eq!(repeat.longest_ticks, Some(15 * MILLISECOND));
        assert_eq!(repeat.covered_ticks, (10 + 11 + 12 + 13 + 14 + 15) * MILLISECOND);
        assert_eq!(repeat.first_start, timeline.start[repeat.activities[0]]);
    }

    #[test]
    fn rows_that_only_say_something_started_or_ended_are_structural() {
        let mut events = vec![
            event("a", "", "A", 0),
            event("a", "", "A", 1),
            event("a", "", "A", 2),
            event("a", "", "A", 3),
        ];
        events[0].message = "Monitored scope start.";
        events[3].message = "Monitored scope end.";
        events[1].message = "Doing the work";
        let table = trace(events);
        let (_, timeline) = build(&table);
        assert_eq!(timeline.structural_rows, vec![true, false, false, true]);

        let columns = TraceColumns::detect(&table);
        let projection = build_projection_with(&table, &columns).expect("a projection");
        let options = TimelineOptions {
            structural_messages: vec!["Doing*".into()],
            ..TimelineOptions::default()
        };
        let custom = Timeline::build(&table, &projection, &columns, &options);
        assert_eq!(custom.structural_rows, vec![false, true, false, false]);
        assert_eq!(
            custom.duration_ticks(0),
            Some(3 * MILLISECOND),
            "structural rows still bound the activity"
        );
    }

    /// Needs the real traces in `fork-docs/samples`, which are not committed.
    /// Run with `cargo test -p kusto_results --lib timeline_of_real_traces -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timeline_of_real_traces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fork-docs/samples");
        for name in ["sample1.ktt", "sample2.ktt"] {
            let Ok(text) = std::fs::read_to_string(root.join(name)) else {
                eprintln!("{name} is not here, skipped");
                continue;
            };
            let result = crate::result::ResultSet::from_json(&text).expect("a result file");
            let table = &result.tables[0];
            let started = std::time::Instant::now();
            let (projection, timeline) = build(table);
            let milliseconds = |ticks: i64| ticks as f64 / MILLISECOND as f64;
            eprintln!("{name}: built in {:?}", started.elapsed());
            let structural = timeline.structural_rows.iter().filter(|row| **row).count();
            eprintln!(
                "  rows {} of which structural {} ({:.0}%), activities {}, outside parent {}, without bounds {}",
                table.rows.len(),
                structural,
                100.0 * structural as f64 / table.rows.len() as f64,
                projection.activities.len(),
                timeline.outside_parent.len(),
                timeline.without_bounds.len()
            );
            let mut untraced: Vec<(usize, i64)> = timeline
                .untraced_ticks
                .iter()
                .enumerate()
                .filter_map(|(index, ticks)| Some((index, (*ticks)?)))
                .collect();
            untraced.sort_by_key(|(_, ticks)| std::cmp::Reverse(*ticks));
            for (index, ticks) in untraced.iter().take(3) {
                eprintln!(
                    "  untraced {:.0} ms of {:.0} ms: {}",
                    milliseconds(*ticks),
                    milliseconds(timeline.duration_ticks(*index).unwrap_or(0)),
                    projection.activities[*index].marker_name.as_deref().unwrap_or("?")
                );
            }
            let mut critical: Vec<(usize, i64)> = timeline
                .critical_ticks
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, ticks)| *ticks > 0)
                .collect();
            critical.sort_by_key(|(_, ticks)| std::cmp::Reverse(*ticks));
            for (index, ticks) in critical.iter().take(3) {
                eprintln!(
                    "  critical {:.0} ms: {}",
                    milliseconds(*ticks),
                    projection.activities[*index].marker_name.as_deref().unwrap_or("?")
                );
            }
            for root in &timeline.roots {
                if let Some(duration) = timeline.duration_ticks(*root) {
                    let branch: i64 = {
                        let mut total = 0;
                        let mut pending = vec![*root];
                        while let Some(next) = pending.pop() {
                            total += timeline.critical_ticks[next];
                            pending.extend(timeline.children[next].iter().copied());
                        }
                        total
                    };
                    assert_eq!(branch, duration, "the critical path covers the root exactly");
                }
            }
            for repeat in timeline.repetitions.iter().take(3) {
                eprintln!(
                    "  repeat x{} covering {:.0} ms: {}",
                    repeat.activities.len(),
                    milliseconds(repeat.covered_ticks),
                    repeat.marker
                );
            }
        }
    }

    /// A grid asks for these when it opens, so the cost over a large table matters.
    /// Run with `cargo test -p kusto_results --release --lib structural_rows_baseline -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing baseline, run in a release build"]
    fn structural_rows_baseline() {
        let messages = ["Monitored scope start.", "Doing the work with a longer message than most", "Monitored scope end.", "Retrying after 5 ms"];
        let table = Table {
            name: "t".into(),
            columns: vec![Column::new("MessageText", "string")],
            rows: (0..500_000)
                .map(|row| vec![Cell::Text(messages[row % messages.len()].into())])
                .collect(),
        };
        let columns = TraceColumns::detect(&table);
        let patterns = TimelineOptions::default().structural_messages;
        let started = std::time::Instant::now();
        let found = structural_rows(&table, &columns, &patterns);
        eprintln!("structural_rows over 500,000 rows: {:?}", started.elapsed());
        assert_eq!(found.iter().filter(|row| **row).count(), 250_000);
    }
}
