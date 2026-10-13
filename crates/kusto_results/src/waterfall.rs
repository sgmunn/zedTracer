//! The waterfall: where the time went in a trace, as one row per activity with a bar from when it
//! started to when it ended.
//!
//! Most activities in a real trace are far too short to see (1,234 of one trace's 1,314 are under
//! a millisecond), so a row's short children are *folded* into it: the row says how many it hides
//! and they open on demand. The chain from a failure to the root is never folded. The critical path
//! needs no protection of its own: an activity that holds a meaningful share of it is at least that
//! long, so it is not short, while the many tiny activities in a sequential run are all "on" the
//! path with no time of their own and are what folding is for.
//!
//! Folding and collapsing are separate. Opening a row's folds adds its short children to the ones
//! it already shows; collapsing a row hides all of its children, the long ones and the failures
//! too, whatever its folds say.
//!
//! This is the model. It says what each row is and where its bar lies; drawing it is up to a view.

use std::collections::HashSet;

use crate::activity::{ActivityProjection, ActivitySeverity};
use crate::failures::Failures;
use crate::result::Table;
use crate::timeline::{Timeline, TimelineOptions};
use crate::trace_schema::TraceColumns;
use crate::trace_text::display_names;
use crate::typed::parse_datetime_ticks;

#[derive(Debug, Clone, PartialEq)]
pub struct WaterfallOptions {
    pub timeline: TimelineOptions,
    /// An activity shorter than this share of the trace's duration is folded into its parent.
    /// 0 folds nothing.
    pub fold_share: f64,
    /// Trailing parts of a process name dropped when it is shortened to show.
    pub generic_actor_suffixes: Vec<String>,
}

impl Default for WaterfallOptions {
    fn default() -> Self {
        Self {
            timeline: TimelineOptions::default(),
            fold_share: 0.01,
            generic_actor_suffixes: vec!["EntryPoint".into(), "Service".into()],
        }
    }
}

/// What the waterfall knows about one activity that has bounds. Times are in 100 ns ticks from
/// the start of the trace.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    pub depth: usize,
    pub marker: Option<String>,
    /// An index into [`Waterfall::actors`].
    pub actor: Option<usize>,
    pub event_count: usize,
    pub first_row: usize,
    pub has_children: bool,
    /// Time its children do not explain, when it has any.
    pub untraced: Option<i64>,
    /// Ticks of its own time on the critical path of its root.
    pub critical: i64,
    pub on_critical_path: bool,
    /// Whether this activity or one below it ended in an error.
    pub on_failure_path: bool,
    /// Whether the failure began here.
    pub origin: bool,
    /// When its error was logged, or when an error it recovered from was.
    pub error_at: Option<i64>,
    pub handled_at: Option<i64>,
    pub severity: Option<ActivitySeverity>,
}

impl Span {
    pub fn duration(&self) -> i64 {
        self.end - self.start
    }
}

/// One row to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub activity: usize,
    pub depth: usize,
    /// How many activities its short children hold, themselves and what is below them, whether
    /// or not they are showing.
    pub folded: usize,
    /// Whether it shows none of its children.
    pub collapsed: bool,
    /// Whether it has children to hide, which are showing or would be if it were not collapsed.
    pub collapsible: bool,
}

/// What has been changed from the default view of the rows, by activity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folds {
    /// Rows whose short children show along with the rest.
    pub opened: HashSet<usize>,
    /// Rows that show none of their children.
    pub collapsed: HashSet<usize>,
}

impl Folds {
    /// Every short activity showing and nothing collapsed.
    pub fn all_open(activities: usize) -> Self {
        Self {
            opened: (0..activities).collect(),
            collapsed: HashSet::new(),
        }
    }

    /// Makes an activity show: its ancestors are no longer collapsed, and a row's folds are opened
    /// only where the activity, or an ancestor it hangs from, is short. The other folds stay as
    /// they were, so a jump to one activity does not also show its short siblings.
    pub fn reveal(&mut self, waterfall: &Waterfall, projection: &ActivityProjection, activity: usize) {
        let mut hidden_by_fold = waterfall
            .span(activity)
            .is_some_and(|span| !waterfall.is_kept(span));
        let mut current = projection.activities.get(activity).and_then(|found| found.parent);
        while let Some(ancestor) = current {
            self.collapsed.remove(&ancestor);
            // An ancestor with no bounds is not a row: what is under it hangs from the row above.
            if let Some(span) = waterfall.span(ancestor) {
                if hidden_by_fold {
                    self.opened.insert(ancestor);
                }
                hidden_by_fold = !waterfall.is_kept(span);
            }
            current = projection.activities.get(ancestor).and_then(|found| found.parent);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Waterfall {
    /// Shortened actor names, indexed by [`Span::actor`].
    pub actors: Vec<String>,
    /// The trace's extent in ticks from its start: `0` to this.
    pub extent: i64,
    /// By activity, `None` for an activity with no readable timestamp, which is not drawn.
    pub spans: Vec<Option<Span>>,
    pub without_bounds: usize,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    /// Activities with bounds in each branch, itself included.
    branch_size: Vec<usize>,
    fold_ticks: i64,
    /// Gaps between children, by activity: the parts of an activity its children do not cover.
    gaps: Vec<Vec<(i64, i64)>>,
}

impl Waterfall {
    /// Builds the waterfall of a trace, or `None` when it has no times to draw.
    pub fn build(
        table: &Table,
        projection: &ActivityProjection,
        columns: &TraceColumns,
        options: &WaterfallOptions,
    ) -> Option<Self> {
        let timestamp = columns.timestamp?;
        let timeline = Timeline::build(table, projection, columns, &options.timeline);
        let failures = Failures::analyze(table, projection, columns, &timeline);
        let origin_ticks = timeline.trace_start?;
        let extent = timeline.trace_end? - origin_ticks;

        let (actors, actor_of) = actors_of(table, projection, columns, options);
        let tick_of_row = |row: usize| -> Option<i64> {
            let cell = table.cell(row, timestamp)?;
            Some(parse_datetime_ticks(&cell.display_text())? - origin_ticks)
        };

        let count = projection.activities.len();
        let mut depths = vec![0usize; count];
        let mut preorder = Vec::with_capacity(count);
        let mut pending: Vec<usize> = timeline.roots.iter().rev().copied().collect();
        while let Some(activity) = pending.pop() {
            preorder.push(activity);
            for child in timeline.children[activity].iter().rev() {
                depths[*child] = depths[activity] + 1;
                pending.push(*child);
            }
        }

        let mut spans: Vec<Option<Span>> = Vec::with_capacity(count);
        for (index, found) in projection.activities.iter().enumerate() {
            let (Some(start), Some(end)) = (timeline.start[index], timeline.end[index]) else {
                spans.push(None);
                continue;
            };
            let has_children = !timeline.children[index].is_empty();
            spans.push(Some(Span {
                start: start - origin_ticks,
                end: end - origin_ticks,
                depth: depths[index],
                marker: found.marker_name.clone(),
                actor: actor_of[index],
                event_count: found.event_rows.len(),
                first_row: found.event_rows.first().copied().unwrap_or(0),
                has_children,
                untraced: timeline.untraced_ticks[index].filter(|_| has_children),
                critical: timeline.critical_ticks[index],
                on_critical_path: timeline.on_critical_path[index],
                on_failure_path: failures.subtree_error[index],
                origin: failures.is_origin(index),
                error_at: failures.error_row[index].and_then(tick_of_row),
                handled_at: failures.handled_row[index].and_then(tick_of_row),
                severity: found.severity,
            }));
        }

        let mut branch_size = vec![0usize; count];
        for activity in preorder.iter().rev() {
            if spans[*activity].is_some() {
                branch_size[*activity] += 1;
            }
            if let Some(parent) = projection.activities[*activity].parent {
                branch_size[parent] += branch_size[*activity];
            }
        }

        let gaps = (0..count)
            .map(|activity| match &spans[activity] {
                Some(span) if span.has_children => {
                    gaps_between(span, &timeline.children[activity], &spans)
                }
                _ => Vec::new(),
            })
            .collect();

        Some(Self {
            actors,
            extent,
            without_bounds: spans.iter().filter(|span| span.is_none()).count(),
            fold_ticks: (extent as f64 * options.fold_share.max(0.0)) as i64,
            children: timeline.children,
            roots: timeline.roots,
            branch_size,
            spans,
            gaps,
        })
    }

    pub fn span(&self, activity: usize) -> Option<&Span> {
        self.spans.get(activity)?.as_ref()
    }

    /// The parts of an activity's interval that none of its children cover, in order. These are
    /// time the trace does not explain.
    pub fn untraced_gaps(&self, activity: usize) -> &[(i64, i64)] {
        self.gaps.get(activity).map_or(&[], Vec::as_slice)
    }

    /// The rows to draw, in tree order. A row's short children are folded into it unless the
    /// row is in `folds.opened`, and an activity is never folded when it is on the way from a
    /// failure to the root. A row in `folds.collapsed` shows none of its children. An activity
    /// with no bounds is skipped and its children take its place.
    pub fn rows(&self, folds: &Folds) -> Vec<Row> {
        let mut rows = Vec::new();
        // (activity, depth, whether it is shown whatever its length)
        let mut pending: Vec<(usize, usize, bool)> =
            self.roots.iter().rev().map(|root| (*root, 0, true)).collect();
        while let Some((activity, depth, forced)) = pending.pop() {
            let Some(span) = self.span(activity) else {
                for child in self.children[activity].iter().rev() {
                    pending.push((*child, depth, forced));
                }
                continue;
            };
            let shown = forced || self.is_kept(span);
            if !shown {
                continue;
            }
            let open = folds.opened.contains(&activity);
            let collapsed = folds.collapsed.contains(&activity);
            let mut folded = 0;
            let mut visible_children = Vec::new();
            for child in &self.children[activity] {
                let kept = match self.span(*child) {
                    Some(child_span) => self.is_kept(child_span),
                    None => true,
                };
                if !kept {
                    folded += self.branch_size[*child];
                }
                if kept || open {
                    visible_children.push(*child);
                }
            }
            rows.push(Row {
                activity,
                depth,
                folded,
                collapsed,
                collapsible: !visible_children.is_empty(),
            });
            if collapsed {
                continue;
            }
            for child in visible_children.into_iter().rev() {
                pending.push((child, depth + 1, open));
            }
        }
        rows
    }

    /// Whether an activity is long enough, or failed, so it is not folded.
    fn is_kept(&self, span: &Span) -> bool {
        span.duration() >= self.fold_ticks || span.on_failure_path
    }
}

/// The parts of an activity's interval outside the union of its children's.
fn gaps_between(span: &Span, children: &[usize], spans: &[Option<Span>]) -> Vec<(i64, i64)> {
    let mut intervals: Vec<(i64, i64)> = children
        .iter()
        .filter_map(|child| {
            let child = spans[*child].as_ref()?;
            let start = child.start.max(span.start);
            let end = child.end.min(span.end);
            (end > start).then_some((start, end))
        })
        .collect();
    intervals.sort_unstable();
    let mut gaps = Vec::new();
    let mut cursor = span.start;
    for (start, end) in intervals {
        if start > cursor {
            gaps.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if span.end > cursor {
        gaps.push((cursor, span.end));
    }
    gaps
}

/// The shortened name of each distinct actor, and which one each activity belongs to.
fn actors_of(
    table: &Table,
    projection: &ActivityProjection,
    columns: &TraceColumns,
    options: &WaterfallOptions,
) -> (Vec<String>, Vec<Option<usize>>) {
    let Some(column) = columns.actor else {
        return (Vec::new(), vec![None; projection.activities.len()]);
    };
    let mut names: Vec<String> = Vec::new();
    let actor_of = projection
        .activities
        .iter()
        .map(|activity| {
            let row = activity.event_rows.first()?;
            let name = table.cell(*row, column)?.display_text().trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(match names.iter().position(|known| *known == name) {
                Some(index) => index,
                None => {
                    names.push(name);
                    names.len() - 1
                }
            })
        })
        .collect();
    let shortened = display_names(&names, &options.generic_actor_suffixes);
    (shortened, actor_of)
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
        actor: &'static str,
        marker: &'static str,
        millis: i64,
        level: i64,
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
            actor: "A",
            marker,
            millis,
            level: 4,
        }
    }

    fn span_of(
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
                        Cell::Text("message".into()),
                    ]
                })
                .collect(),
        }
    }

    fn build(table: &Table) -> (ActivityProjection, Waterfall) {
        let columns = TraceColumns::detect(table);
        let projection = build_projection_with(table, &columns).expect("a projection");
        let waterfall = Waterfall::build(table, &projection, &columns, &WaterfallOptions::default())
            .expect("a waterfall");
        (projection, waterfall)
    }

    fn index(projection: &ActivityProjection, id: &str) -> usize {
        projection
            .activities
            .iter()
            .position(|activity| activity.activity_id == id)
            .expect("the activity exists")
    }

    fn shown(waterfall: &Waterfall, projection: &ActivityProjection) -> Vec<String> {
        shown_with(waterfall, projection, &Folds::default())
    }

    fn shown_with(waterfall: &Waterfall, projection: &ActivityProjection, folds: &Folds) -> Vec<String> {
        waterfall
            .rows(folds)
            .iter()
            .map(|row| projection.activities[row.activity].activity_id.clone())
            .collect()
    }

    #[test]
    fn rows_follow_the_tree_with_bars_from_the_start_of_the_trace() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 100, 1100);
        span_of(&mut events, "a", "r", "First", 150, 650);
        span_of(&mut events, "b", "r", "Second", 700, 1000);
        span_of(&mut events, "a1", "a", "Inner", 200, 400);
        let (projection, waterfall) = build(&trace(events));
        let rows = waterfall.rows(&Folds::default());
        let order: Vec<(&str, usize)> = rows
            .iter()
            .map(|row| (projection.activities[row.activity].activity_id.as_str(), row.depth))
            .collect();
        assert_eq!(order, vec![("r", 0), ("a", 1), ("a1", 2), ("b", 1)]);
        let a = waterfall.span(index(&projection, "a")).expect("a span");
        assert_eq!((a.start, a.end), (50 * MILLISECOND, 550 * MILLISECOND));
        assert_eq!(a.duration(), 500 * MILLISECOND);
        assert_eq!(waterfall.extent, 1000 * MILLISECOND);
    }

    #[test]
    fn short_activities_fold_into_their_parent_and_open_on_demand() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "big", "r", "Big", 100, 600);
        span_of(&mut events, "tiny1", "r", "Tiny", 150, 155);
        span_of(&mut events, "tiny2", "r", "Tiny", 200, 204);
        span_of(&mut events, "deep", "tiny2", "Deeper", 201, 203);
        let (projection, waterfall) = build(&trace(events));
        assert_eq!(shown(&waterfall, &projection), vec!["r", "big"]);
        let rows = waterfall.rows(&Folds::default());
        assert_eq!(rows[0].folded, 3, "tiny1, tiny2 and what is below it");
        assert_eq!(rows[1].folded, 0);

        let folds = Folds {
            opened: [index(&projection, "r")].into_iter().collect(),
            ..Folds::default()
        };
        assert_eq!(
            shown_with(&waterfall, &projection, &folds),
            vec!["r", "big", "tiny1", "tiny2"],
            "deep is still folded into tiny2"
        );
        let rows = waterfall.rows(&folds);
        assert_eq!(rows[0].folded, 3, "the count does not change when they show");
        assert_eq!(rows[3].folded, 1);
    }

    #[test]
    fn collapsing_a_row_hides_all_its_children_and_leaves_its_folds_as_they_were() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "big", "r", "Big", 100, 600);
        span_of(&mut events, "inner", "big", "Inner", 150, 550);
        span_of(&mut events, "tiny", "r", "Tiny", 700, 704);
        let (projection, waterfall) = build(&trace(events));
        let r = index(&projection, "r");
        assert_eq!(shown(&waterfall, &projection), vec!["r", "big", "inner"]);

        let mut folds = Folds::default();
        folds.collapsed.insert(r);
        assert_eq!(shown_with(&waterfall, &projection, &folds), vec!["r"]);
        let rows = waterfall.rows(&folds);
        assert!(rows[0].collapsed && rows[0].collapsible);
        assert_eq!(rows[0].folded, 1, "tiny is still behind its fold");

        folds.collapsed.clear();
        let rows = waterfall.rows(&folds);
        assert!(!rows[0].collapsed);
        assert_eq!(rows.len(), 3);

        folds.collapsed.insert(index(&projection, "big"));
        assert_eq!(shown_with(&waterfall, &projection, &folds), vec!["r", "big"], "only below Big");
    }

    #[test]
    fn opening_the_folds_of_a_collapsed_row_shows_nothing_until_it_is_expanded() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "big", "r", "Big", 100, 600);
        span_of(&mut events, "tiny", "r", "Tiny", 700, 704);
        let (projection, waterfall) = build(&trace(events));
        let r = index(&projection, "r");
        let mut folds = Folds {
            opened: [r].into_iter().collect(),
            collapsed: [r].into_iter().collect(),
        };
        assert_eq!(shown_with(&waterfall, &projection, &folds), vec!["r"]);
        folds.collapsed.clear();
        assert_eq!(shown_with(&waterfall, &projection, &folds), vec!["r", "big", "tiny"]);
    }

    #[test]
    fn a_row_with_only_short_children_has_nothing_to_collapse_until_they_are_opened() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "tiny", "r", "Tiny", 700, 704);
        let (projection, waterfall) = build(&trace(events));
        let rows = waterfall.rows(&Folds::default());
        assert_eq!((rows[0].folded, rows[0].collapsible), (1, false));
        assert_eq!(rows.len(), 1);

        let folds = Folds {
            opened: [index(&projection, "r")].into_iter().collect(),
            ..Folds::default()
        };
        let rows = waterfall.rows(&folds);
        assert_eq!((rows[0].folded, rows[0].collapsible), (1, true));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn revealing_expands_what_is_above_and_opens_only_the_folds_that_hide_the_activity() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "big", "r", "Big", 100, 600);
        span_of(&mut events, "a", "big", "A", 110, 115);
        span_of(&mut events, "b", "big", "B", 120, 125);
        span_of(&mut events, "b1", "b", "B1", 121, 123);
        span_of(&mut events, "tiny", "r", "Tiny", 700, 704);
        let (projection, waterfall) = build(&trace(events));
        let (r, big, b) = (index(&projection, "r"), index(&projection, "big"), index(&projection, "b"));

        let mut folds = Folds {
            collapsed: [r, big].into_iter().collect(),
            ..Folds::default()
        };
        folds.reveal(&waterfall, &projection, index(&projection, "b1"));
        assert!(folds.collapsed.is_empty());
        assert_eq!(folds.opened, [big, b].into_iter().collect(), "B hangs from Big and B1 from B");
        assert_eq!(
            shown_with(&waterfall, &projection, &folds),
            vec!["r", "big", "a", "b", "b1"],
            "Tiny, which holds nothing Big does not, stays folded"
        );

        let mut folds = Folds {
            collapsed: [r, big].into_iter().collect(),
            ..Folds::default()
        };
        folds.reveal(&waterfall, &projection, big);
        assert!(folds.opened.is_empty(), "Big is long, so nothing hides it but the collapse of Run");
        assert_eq!(folds.collapsed, [big].into_iter().collect(), "Big itself is not above Big");
    }

    #[test]
    fn revealing_through_an_activity_with_no_timestamp_opens_the_row_it_hangs_from() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        events.push(event("silent", "r", "Silent", 100));
        span_of(&mut events, "kid", "silent", "Kid", 300, 303);
        let mut table = trace(events);
        for row in &mut table.rows {
            if row[0] == Cell::Text("silent".into()) {
                row[4] = Cell::Text("not a time".into());
            }
        }
        let (projection, waterfall) = build(&table);
        assert_eq!(shown(&waterfall, &projection), vec!["r"]);

        let mut folds = Folds::default();
        folds.reveal(&waterfall, &projection, index(&projection, "kid"));
        assert_eq!(folds.opened, [index(&projection, "r")].into_iter().collect());
        assert_eq!(shown_with(&waterfall, &projection, &folds), vec!["r", "kid"]);
    }

    #[test]
    fn a_failure_is_never_folded_but_a_short_activity_on_the_critical_path_is() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "big", "r", "Big", 0, 900);
        span_of(&mut events, "late", "r", "Late", 995, 1000);
        span_of(&mut events, "quiet", "r", "Quiet", 500, 505);
        let mut failing = event("bad", "r", "Bad", 930);
        failing.level = 2;
        events.push(failing);
        let (projection, waterfall) = build(&trace(events));
        let names = shown(&waterfall, &projection);
        assert!(!names.contains(&"late".to_string()), "5 ms is short however late it ends: {names:?}");
        let late = waterfall.span(index(&projection, "late")).expect("a span");
        assert!(late.on_critical_path, "it ends the trace, so the path reaches it");
        assert!(names.contains(&"bad".to_string()), "a failure origin: {names:?}");
        assert!(!names.contains(&"quiet".to_string()), "short, in parallel with Big and fine: {names:?}");
        let bad = waterfall.span(index(&projection, "bad")).expect("a span");
        assert!(bad.origin && bad.on_failure_path);
        assert_eq!(bad.error_at, Some(930 * MILLISECOND));
        assert!(waterfall.span(index(&projection, "r")).expect("a span").on_failure_path);
    }

    #[test]
    fn a_parent_shows_the_time_its_children_do_not_cover() {
        let mut events = Vec::new();
        span_of(&mut events, "p", "", "Parent", 0, 100);
        span_of(&mut events, "a", "p", "A", 10, 30);
        span_of(&mut events, "b", "p", "B", 20, 50);
        let (projection, waterfall) = build(&trace(events));
        let p = index(&projection, "p");
        assert_eq!(
            waterfall.untraced_gaps(p),
            [(0, 10 * MILLISECOND), (50 * MILLISECOND, 100 * MILLISECOND)]
        );
        assert_eq!(waterfall.span(p).and_then(|span| span.untraced), Some(60 * MILLISECOND));
        assert!(waterfall.untraced_gaps(index(&projection, "a")).is_empty());
        assert_eq!(waterfall.span(index(&projection, "a")).and_then(|span| span.untraced), None, "a leaf has none");
    }

    #[test]
    fn a_handled_error_is_marked_apart_from_a_failure() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 100);
        let mut retry = event("a", "r", "Try", 10);
        retry.level = 2;
        events.push(retry);
        events.push(event("a", "r", "Try", 20));
        let (projection, waterfall) = build(&trace(events));
        let a = waterfall.span(index(&projection, "a")).expect("a span");
        assert_eq!(a.handled_at, Some(10 * MILLISECOND));
        assert_eq!(a.error_at, None);
        assert!(!a.on_failure_path);
    }

    #[test]
    fn actors_are_shown_with_short_names_and_activities_point_at_them() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 100);
        let mut other = event("a", "r", "Ask", 10);
        other.actor = "Microsoft.Foo.Beta.Service";
        events.push(other);
        for event in &mut events {
            if event.actor == "A" {
                event.actor = "Microsoft.Foo.Alpha.Service";
            }
        }
        let (projection, waterfall) = build(&trace(events));
        assert_eq!(waterfall.actors, vec!["Alpha", "Beta"]);
        assert_eq!(waterfall.span(index(&projection, "r")).and_then(|span| span.actor), Some(0));
        assert_eq!(waterfall.span(index(&projection, "a")).and_then(|span| span.actor), Some(1));
    }

    #[test]
    fn nothing_folds_when_the_share_is_zero_and_without_times_there_is_no_waterfall() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        span_of(&mut events, "tiny", "r", "Tiny", 500, 501);
        let table = trace(events);
        let columns = TraceColumns::detect(&table);
        let projection = build_projection_with(&table, &columns).expect("a projection");
        let options = WaterfallOptions {
            fold_share: 0.0,
            ..WaterfallOptions::default()
        };
        let waterfall = Waterfall::build(&table, &projection, &columns, &options).expect("a waterfall");
        assert_eq!(waterfall.rows(&Folds::default()).len(), 2);

        let mut untimed = table;
        untimed.columns.remove(4);
        for row in &mut untimed.rows {
            row.remove(4);
        }
        let columns = TraceColumns::detect(&untimed);
        let projection = build_projection_with(&untimed, &columns).expect("a projection");
        assert!(Waterfall::build(&untimed, &projection, &columns, &WaterfallOptions::default()).is_none());
    }

    #[test]
    fn an_activity_with_no_timestamp_is_skipped_and_its_children_take_its_place() {
        let mut events = Vec::new();
        span_of(&mut events, "r", "", "Run", 0, 1000);
        events.push(event("silent", "r", "Silent", 100));
        span_of(&mut events, "kid", "silent", "Kid", 200, 800);
        let mut table = trace(events);
        for row in &mut table.rows {
            if row[0] == Cell::Text("silent".into()) {
                row[4] = Cell::Text("not a time".into());
            }
        }
        let (projection, waterfall) = build(&table);
        assert_eq!(waterfall.without_bounds, 1);
        let rows = waterfall.rows(&Folds::default());
        let named: Vec<(String, usize)> = rows
            .iter()
            .map(|row| (projection.activities[row.activity].activity_id.clone(), row.depth))
            .collect();
        assert_eq!(named, vec![("r".to_string(), 0), ("kid".to_string(), 1)]);
    }

    #[test]
    fn a_very_deep_chain_does_not_overflow_the_stack() {
        let ids: Vec<&'static str> = (0..20_000)
            .map(|number| &*Box::leak(format!("n{number}").into_boxed_str()))
            .collect();
        let mut events = Vec::new();
        for (number, id) in ids.iter().enumerate() {
            let parent = if number == 0 { "" } else { ids[number - 1] };
            span_of(&mut events, id, parent, "Hop", number as i64, 100_000 - number as i64);
        }
        let (_, waterfall) = build(&trace(events));
        assert_eq!(waterfall.rows(&Folds::default()).len(), 20_000);
    }

    /// Needs the real traces in `fork-docs/samples`, which are not committed.
    /// Run with `cargo test -p kusto_results --lib waterfall_of_real_traces -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn waterfall_of_real_traces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fork-docs/samples");
        for name in ["sample1.ktt", "sample2.ktt"] {
            let Ok(text) = std::fs::read_to_string(root.join(name)) else {
                eprintln!("{name} is not here, skipped");
                continue;
            };
            let result = crate::result::ResultSet::from_json(&text).expect("a result file");
            let table = &result.tables[0];
            let started = std::time::Instant::now();
            let (projection, waterfall) = build(table);
            let rows = waterfall.rows(&Folds::default());
            eprintln!(
                "{name}: {} activities, {} rows at 1% (built and folded in {:?}), extent {:.0} ms",
                projection.activities.len(),
                rows.len(),
                started.elapsed(),
                waterfall.extent as f64 / MILLISECOND as f64
            );
            for row in rows.iter().take(14) {
                let span = waterfall.span(row.activity).expect("a span");
                eprintln!(
                    "  {}{} {:.0} ms at +{:.0}{}",
                    "  ".repeat(row.depth),
                    span.marker.as_deref().unwrap_or("?"),
                    span.duration() as f64 / MILLISECOND as f64,
                    span.start as f64 / MILLISECOND as f64,
                    if row.folded > 0 { format!(" [{} folded]", row.folded) } else { String::new() }
                );
            }
            let all = Waterfall::build(
                table,
                &projection,
                &TraceColumns::detect(table),
                &WaterfallOptions { fold_share: 0.0, ..WaterfallOptions::default() },
            )
            .expect("a waterfall");
            eprintln!("  with nothing folded: {} rows", all.rows(&Folds::default()).len());
        }
    }
}
