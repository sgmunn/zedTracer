//! The structured activity view: trace rows grouped into activities and arranged as a tree.
//!
//! Rows sharing a `CurrentActivityId` form one activity and stay individual events. Each
//! activity's parent comes from `ParentActivityId`. The result is a forest, and a problem in
//! the data (a missing, conflicting or circular parent) never drops a row: the activity
//! becomes a root and is marked.

use std::collections::HashMap;

use crate::result::{Cell, Table};
use crate::trace_schema::TraceColumns;
use crate::view::{SeverityLevel, severity_level};

pub const MISSING_ACTIVITY_LABEL: &str = "(missing CurrentActivityId)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyIssue {
    /// The named parent is not in the result.
    Orphan,
    /// The activity's rows name more than one parent, so none is chosen.
    ConflictingParents,
    /// The activity's parent chain loops; the loop is broken at its earliest-seen activity.
    Cycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Full,
    /// A handled issue: an earlier warning or error followed by a normal final event.
    Muted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivitySeverity {
    pub level: SeverityLevel,
    pub strength: Strength,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub activity_id: String,
    /// False for a row that had no `CurrentActivityId` and became an activity of its own.
    pub has_activity_id: bool,
    /// Source rows of this activity's own events, in source order.
    pub event_rows: Vec<usize>,
    pub parent: Option<usize>,
    pub issue: Option<HierarchyIssue>,
    pub severity: Option<ActivitySeverity>,
    /// The `MarkerName` of the first event, when the table has that column.
    pub marker_name: Option<String>,
    pub child_count: usize,
    /// Activities in this branch, this one included.
    pub subtree_activity_count: usize,
    /// Length of the longest chain below this activity; a leaf is 0.
    pub max_descendant_depth: usize,
    /// Tree level of this activity; a root is 0.
    pub depth: usize,
}

impl Activity {
    /// The warning triangle marks an activity's own warning, error or critical event, final or
    /// earlier. Hierarchy problems do not show it.
    pub fn shows_warning_triangle(&self) -> bool {
        self.severity.is_some_and(|severity| severity.level <= 3)
    }
}

/// One row of the tree in presentation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityRow {
    pub source_row: usize,
    pub depth: usize,
    pub first_in_activity: bool,
    pub activity: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityProjection {
    pub current_activity_column: usize,
    pub parent_activity_column: usize,
    pub activities: Vec<Activity>,
    /// Depth-first: each activity's first event, then its child activities, then its other
    /// events, so a high-volume parent does not bury its children.
    pub rows: Vec<ActivityRow>,
}

impl ActivityProjection {
    /// The deepest activities, in first-seen order, and their depth. The Deepest action cycles
    /// through them.
    pub fn deepest(&self) -> (usize, Vec<usize>) {
        let deepest = self
            .activities
            .iter()
            .map(|activity| activity.depth)
            .max()
            .unwrap_or(0);
        let tied = self
            .activities
            .iter()
            .enumerate()
            .filter(|(_, activity)| activity.depth == deepest)
            .map(|(index, _)| index)
            .collect();
        (deepest, tied)
    }

    /// Activity indexes from the root down to `activity`, for expanding its ancestors.
    pub fn ancestors(&self, activity: usize) -> Vec<usize> {
        let mut chain = Vec::new();
        let mut current = self.activities.get(activity).and_then(|found| found.parent);
        while let Some(parent) = current {
            chain.push(parent);
            current = self.activities.get(parent).and_then(|found| found.parent);
        }
        chain.reverse();
        chain
    }
}

struct Group {
    id: String,
    has_activity_id: bool,
    rows: Vec<usize>,
    parent_candidates: Vec<String>,
    parent: Option<usize>,
    issue: Option<HierarchyIssue>,
}

/// One activity of a trace and everything below it, which every view can be rebuilt from.
#[derive(Debug, Clone, PartialEq)]
pub struct Focus {
    pub activity_id: String,
    pub marker: Option<String>,
    /// The activity's parent, to focus one level up; `None` for a root.
    pub parent_id: Option<String>,
    /// The source rows of the events of the activity and of everything below it, in source order.
    pub rows: Vec<usize>,
}

pub enum FocusTarget<'a> {
    /// An activity id, matched exactly and then ignoring case, with padding trimmed.
    Id(&'a str),
    /// The activity that owns a source row.
    Row(usize),
}

/// Finds the activity a target names and what is below it, or `None` when there is no such
/// activity or it has no id of its own to focus on.
pub fn resolve_focus(
    table: &Table,
    columns: &TraceColumns,
    target: FocusTarget<'_>,
) -> Option<Focus> {
    let projection = build_projection_with(table, columns)?;
    let activities = &projection.activities;
    let chosen = match target {
        FocusTarget::Id(text) => {
            let text = text.trim();
            (0..activities.len())
                .filter(|index| activities[*index].has_activity_id)
                .find(|index| activities[*index].activity_id == text)
                .or_else(|| {
                    (0..activities.len())
                        .filter(|index| activities[*index].has_activity_id)
                        .find(|index| activities[*index].activity_id.eq_ignore_ascii_case(text))
                })?
        }
        FocusTarget::Row(row) => (0..activities.len())
            .find(|index| activities[*index].event_rows.contains(&row))
            .filter(|index| activities[*index].has_activity_id)?,
    };

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); activities.len()];
    for (index, activity) in activities.iter().enumerate() {
        if let Some(parent) = activity.parent {
            children[parent].push(index);
        }
    }
    let mut rows = Vec::new();
    let mut pending = vec![chosen];
    while let Some(activity) = pending.pop() {
        rows.extend(activities[activity].event_rows.iter().copied());
        pending.extend(children[activity].iter().copied());
    }
    rows.sort_unstable();
    Some(Focus {
        activity_id: activities[chosen].activity_id.clone(),
        marker: activities[chosen].marker_name.clone(),
        parent_id: activities[chosen]
            .parent
            .filter(|parent| activities[*parent].has_activity_id)
            .map(|parent| activities[parent].activity_id.clone()),
        rows,
    })
}

/// Whether the table has the two columns the structured view needs, under the built-in
/// names. Cheap, unlike building the projection.
pub fn has_activity_columns(table: &Table) -> bool {
    TraceColumns::detect(table).supports_activity()
}

/// Builds the projection with the built-in column names.
pub fn build_projection(table: &Table) -> Option<ActivityProjection> {
    build_projection_with(table, &TraceColumns::detect(table))
}

/// Builds the projection from the columns a schema resolved, or `None` when the table lacks
/// either activity column.
pub fn build_projection_with(table: &Table, columns: &TraceColumns) -> Option<ActivityProjection> {
    build_projection_in(table, columns, None)
}

/// Builds the projection of the part of a trace a focus names, or of all of it with `None`. The
/// events keep their source rows, and the focused activity is a root with no hierarchy issue:
/// that its caller is outside the focus is the point of focusing, not a problem in the data.
pub fn build_projection_in(
    table: &Table,
    columns: &TraceColumns,
    focus: Option<&Focus>,
) -> Option<ActivityProjection> {
    match focus {
        Some(focus) => build_from_rows(
            table,
            columns,
            &mut focus.rows.iter().copied(),
            Some(focus.activity_id.as_str()),
        ),
        None => build_from_rows(table, columns, &mut (0..table.rows.len()), None),
    }
}

fn build_from_rows(
    table: &Table,
    columns: &TraceColumns,
    row_indexes: &mut dyn Iterator<Item = usize>,
    root: Option<&str>,
) -> Option<ActivityProjection> {
    let current_column = columns.activity_id?;
    let parent_column = columns.parent_activity_id?;
    let severity_column = columns.severity;
    let marker_column = columns.marker;

    let mut groups: Vec<Group> = Vec::new();
    let mut group_by_id: HashMap<String, usize> = HashMap::new();
    for row_index in row_indexes {
        let Some(row) = table.rows.get(row_index) else {
            continue;
        };
        let activity_id = identifier(row.get(current_column));
        let group_index = match activity_id.as_ref().and_then(|id| group_by_id.get(id)) {
            Some(existing) => *existing,
            None => {
                groups.push(Group {
                    id: activity_id
                        .clone()
                        .unwrap_or_else(|| MISSING_ACTIVITY_LABEL.to_string()),
                    has_activity_id: activity_id.is_some(),
                    rows: Vec::new(),
                    parent_candidates: Vec::new(),
                    parent: None,
                    issue: None,
                });
                if let Some(id) = activity_id.clone() {
                    group_by_id.insert(id, groups.len() - 1);
                }
                groups.len() - 1
            }
        };
        groups[group_index].rows.push(row_index);
        if activity_id.is_some() {
            if let Some(parent_id) = identifier(row.get(parent_column)) {
                if !groups[group_index].parent_candidates.contains(&parent_id) {
                    groups[group_index].parent_candidates.push(parent_id);
                }
            }
        }
    }

    for group in &mut groups {
        if root.is_some_and(|root| group.has_activity_id && group.id == root) {
            group.parent_candidates.clear();
        }
        match group.parent_candidates.as_slice() {
            [] => {}
            [only] => match group_by_id.get(only) {
                Some(parent) => group.parent = Some(*parent),
                None => group.issue = Some(HierarchyIssue::Orphan),
            },
            _ => group.issue = Some(HierarchyIssue::ConflictingParents),
        }
    }
    break_cycles(&mut groups);

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); groups.len()];
    for (index, group) in groups.iter().enumerate() {
        if let Some(parent) = group.parent {
            children[parent].push(index);
        }
    }

    let (rows, depths, preorder) = arrange(&groups, &children);

    let mut subtree_counts = vec![1usize; groups.len()];
    let mut descendant_depths = vec![0usize; groups.len()];
    for &index in preorder.iter().rev() {
        if let Some(parent) = groups[index].parent {
            subtree_counts[parent] += subtree_counts[index];
            descendant_depths[parent] = descendant_depths[parent].max(descendant_depths[index] + 1);
        }
    }

    let activities = groups
        .iter()
        .enumerate()
        .map(|(index, group)| Activity {
            activity_id: group.id.clone(),
            has_activity_id: group.has_activity_id,
            event_rows: group.rows.clone(),
            parent: group.parent,
            issue: group.issue,
            severity: severity_column
                .and_then(|column| severity_outcome(table, &group.rows, column)),
            marker_name: marker_column.and_then(|column| {
                let first = *group.rows.first()?;
                let text = table.cell(first, column)?.display_text();
                let text = text.trim();
                (!text.is_empty()).then(|| text.to_string())
            }),
            child_count: children[index].len(),
            subtree_activity_count: subtree_counts[index],
            max_descendant_depth: descendant_depths[index],
            depth: depths[index],
        })
        .collect();

    Some(ActivityProjection {
        current_activity_column: current_column,
        parent_activity_column: parent_column,
        activities,
        rows,
    })
}

fn identifier(cell: Option<&Cell>) -> Option<String> {
    let text = cell?.display_text();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Breaks every parent loop at the activity seen first, leaving the rest of the branch intact.
/// Each activity has at most one parent, so loops cannot overlap and one pass finds them all.
fn break_cycles(groups: &mut [Group]) {
    const UNVISITED: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;
    let mut state = vec![UNVISITED; groups.len()];
    for start in 0..groups.len() {
        if state[start] != UNVISITED {
            continue;
        }
        let mut path: Vec<usize> = Vec::new();
        let mut current = start;
        loop {
            match state[current] {
                DONE => break,
                ON_PATH => {
                    if let Some(position) = path.iter().position(|node| *node == current) {
                        if let Some(&earliest) = path[position..].iter().min() {
                            groups[earliest].parent = None;
                            groups[earliest].issue = Some(HierarchyIssue::Cycle);
                        }
                    }
                    break;
                }
                _ => {}
            }
            state[current] = ON_PATH;
            path.push(current);
            match groups[current].parent {
                Some(parent) => current = parent,
                None => break,
            }
        }
        for node in path {
            state[node] = DONE;
        }
    }
}

enum Step {
    Enter(usize, usize),
    Remaining(usize, usize),
}

/// Presentation rows, each activity's depth, and the activities in depth-first order.
fn arrange(
    groups: &[Group],
    children: &[Vec<usize>],
) -> (Vec<ActivityRow>, Vec<usize>, Vec<usize>) {
    let mut rows = Vec::with_capacity(groups.iter().map(|group| group.rows.len()).sum());
    let mut depths = vec![0usize; groups.len()];
    let mut preorder = Vec::with_capacity(groups.len());
    let mut stack: Vec<Step> = groups
        .iter()
        .enumerate()
        .filter(|(_, group)| group.parent.is_none())
        .map(|(index, _)| Step::Enter(index, 0))
        .rev()
        .collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(index, depth) => {
                depths[index] = depth;
                preorder.push(index);
                if let Some(first) = groups[index].rows.first() {
                    rows.push(ActivityRow {
                        source_row: *first,
                        depth,
                        first_in_activity: true,
                        activity: index,
                    });
                }
                stack.push(Step::Remaining(index, depth));
                for child in children[index].iter().rev() {
                    stack.push(Step::Enter(*child, depth + 1));
                }
            }
            Step::Remaining(index, depth) => {
                for source_row in groups[index].rows.iter().skip(1) {
                    rows.push(ActivityRow {
                        source_row: *source_row,
                        depth,
                        first_in_activity: false,
                        activity: index,
                    });
                }
            }
        }
    }
    (rows, depths, preorder)
}

/// The activity's own outcome: the final event decides, unless it is normal, verbose or
/// unreadable and an earlier event was a warning or worse, which shows as a muted outcome.
fn severity_outcome(table: &Table, rows: &[usize], column: usize) -> Option<ActivitySeverity> {
    let (&last, earlier) = rows.split_last()?;
    let final_level = severity_level(table.cell(last, column));
    if let Some(level) = final_level.filter(|level| *level <= 3) {
        return Some(ActivitySeverity {
            level,
            strength: Strength::Full,
        });
    }
    let worst_earlier = earlier
        .iter()
        .filter_map(|row| severity_level(table.cell(*row, column)))
        .filter(|level| *level <= 3)
        .min();
    if let Some(level) = worst_earlier {
        return Some(ActivitySeverity {
            level,
            strength: Strength::Muted,
        });
    }
    final_level.map(|level| ActivitySeverity {
        level,
        strength: Strength::Full,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Column;

    fn trace(events: &[(&str, &str, i64)]) -> Table {
        Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("Level", "long"),
                Column::new("MarkerName", "string"),
            ],
            rows: events
                .iter()
                .map(|(current, parent, level)| {
                    vec![
                        if current.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Text((*current).into())
                        },
                        if parent.is_empty() {
                            Cell::Null
                        } else {
                            Cell::Text((*parent).into())
                        },
                        Cell::Int(*level),
                        Cell::Text(format!("marker {current}")),
                    ]
                })
                .collect(),
        }
    }

    fn find<'a>(projection: &'a ActivityProjection, id: &str) -> &'a Activity {
        projection
            .activities
            .iter()
            .find(|activity| activity.activity_id == id)
            .unwrap()
    }

    #[test]
    fn needs_both_activity_columns() {
        let mut table = trace(&[("a", "", 4)]);
        table.columns.remove(1);
        for row in &mut table.rows {
            row.remove(1);
        }
        assert!(build_projection(&table).is_none());
    }

    #[test]
    fn groups_events_and_orders_children_before_remaining_events() {
        let table = trace(&[
            ("a", "", 4),
            ("b", "a", 4),
            ("a", "", 4),
            ("c", "a", 4),
            ("b", "a", 4),
        ]);
        let projection = build_projection(&table).unwrap();
        let order: Vec<(usize, usize)> = projection
            .rows
            .iter()
            .map(|row| (row.source_row, row.depth))
            .collect();
        assert_eq!(order, [(0, 0), (1, 1), (4, 1), (3, 1), (2, 0)]);
        let a = find(&projection, "a");
        assert_eq!(a.event_rows, [0, 2]);
        assert_eq!(a.child_count, 2);
        assert_eq!(a.subtree_activity_count, 3);
        assert_eq!(a.max_descendant_depth, 1);
        assert_eq!(a.marker_name.as_deref(), Some("marker a"));
    }

    #[test]
    fn supports_several_roots_in_first_seen_order() {
        let table = trace(&[("b", "", 4), ("a", "", 4), ("c", "b", 4)]);
        let projection = build_projection(&table).unwrap();
        let roots: Vec<&str> = projection
            .activities
            .iter()
            .filter(|activity| activity.parent.is_none())
            .map(|activity| activity.activity_id.as_str())
            .collect();
        assert_eq!(roots, ["b", "a"]);
    }

    #[test]
    fn a_missing_parent_makes_a_marked_root() {
        let projection = build_projection(&trace(&[("a", "ghost", 4)])).unwrap();
        let a = find(&projection, "a");
        assert_eq!((a.parent, a.issue), (None, Some(HierarchyIssue::Orphan)));
        assert!(!a.shows_warning_triangle());
    }

    #[test]
    fn conflicting_parents_are_not_guessed() {
        let table = trace(&[("p", "", 4), ("q", "", 4), ("a", "p", 4), ("a", "q", 4)]);
        let projection = build_projection(&table).unwrap();
        let a = find(&projection, "a");
        assert_eq!(a.issue, Some(HierarchyIssue::ConflictingParents));
        assert_eq!(a.parent, None);
    }

    #[test]
    fn a_cycle_is_broken_at_the_earliest_seen_activity_without_dropping_rows() {
        let table = trace(&[("a", "b", 4), ("b", "a", 4), ("s", "s", 4)]);
        let projection = build_projection(&table).unwrap();
        assert_eq!(find(&projection, "a").issue, Some(HierarchyIssue::Cycle));
        assert_eq!(find(&projection, "a").parent, None);
        assert_eq!(find(&projection, "b").parent, Some(0));
        assert_eq!(find(&projection, "s").issue, Some(HierarchyIssue::Cycle));
        assert_eq!(projection.rows.len(), 3);
    }

    #[test]
    fn rows_without_an_activity_id_are_their_own_roots() {
        let table = trace(&[("", "", 4), ("", "x", 3)]);
        let projection = build_projection(&table).unwrap();
        assert_eq!(projection.activities.len(), 2);
        assert!(
            projection
                .activities
                .iter()
                .all(|activity| !activity.has_activity_id
                    && activity.parent.is_none()
                    && activity.activity_id == MISSING_ACTIVITY_LABEL)
        );
    }

    #[test]
    fn reports_depth_and_the_deepest_activities() {
        let table = trace(&[
            ("r", "", 4),
            ("a", "r", 4),
            ("b", "a", 4),
            ("c", "r", 4),
            ("d", "c", 4),
        ]);
        let projection = build_projection(&table).unwrap();
        assert_eq!(find(&projection, "r").max_descendant_depth, 2);
        assert_eq!(find(&projection, "b").depth, 2);
        assert_eq!(projection.deepest(), (2, vec![2, 4]));
        assert_eq!(projection.ancestors(2), [0, 1]);
    }

    fn outcome(levels: &[i64]) -> (Option<ActivitySeverity>, bool) {
        let events: Vec<(&str, &str, i64)> = levels.iter().map(|level| ("a", "", *level)).collect();
        let projection = build_projection(&trace(&events)).unwrap();
        let activity = &projection.activities[0];
        (activity.severity, activity.shows_warning_triangle())
    }

    fn sev(level: u8, strength: Strength) -> Option<ActivitySeverity> {
        Some(ActivitySeverity { level, strength })
    }

    #[test]
    fn the_final_event_decides_unless_it_is_normal_after_an_earlier_issue() {
        assert_eq!(outcome(&[4, 2]), (sev(2, Strength::Full), true));
        assert_eq!(outcome(&[1, 3]), (sev(3, Strength::Full), true));
        assert_eq!(outcome(&[3, 4]), (sev(3, Strength::Muted), true));
        assert_eq!(outcome(&[3, 2, 4]), (sev(2, Strength::Muted), true));
        assert_eq!(outcome(&[2, 5]), (sev(2, Strength::Muted), true));
        assert_eq!(outcome(&[4, 5]), (sev(5, Strength::Full), false));
        assert_eq!(outcome(&[5, 4]), (sev(4, Strength::Full), false));
    }

    #[test]
    fn unreadable_levels_leave_the_activity_uncoloured_or_muted() {
        assert_eq!(outcome(&[9, 0]), (None, false));
        assert_eq!(outcome(&[3, 9]), (sev(3, Strength::Muted), true));
    }

    #[test]
    fn a_child_never_changes_its_parents_colour() {
        let table = trace(&[("p", "", 4), ("c", "p", 2), ("p", "", 4)]);
        let projection = build_projection(&table).unwrap();
        assert_eq!(find(&projection, "p").severity, sev(4, Strength::Full));
        assert_eq!(find(&projection, "c").severity, sev(2, Strength::Full));
    }

    #[test]
    fn a_deep_chain_does_not_overflow_the_stack() {
        let ids: Vec<String> = (0..50_000).map(|index| format!("a{index}")).collect();
        let events: Vec<(&str, &str, i64)> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    id.as_str(),
                    if index == 0 {
                        ""
                    } else {
                        ids[index - 1].as_str()
                    },
                    4,
                )
            })
            .collect();
        let projection = build_projection(&trace(&events)).unwrap();
        assert_eq!(projection.activities[0].max_descendant_depth, 49_999);
    }

    /// r is the root with children a and b; c is below a; z is another root.
    fn tree() -> Table {
        let mut table = trace(&[
            ("r", "", 4),
            ("a", "r", 4),
            ("c", "a", 4),
            ("b", "r", 4),
            ("a", "r", 3),
            ("z", "", 4),
            ("r", "", 4),
        ]);
        table.rows.push(vec![Cell::Null, Cell::Null, Cell::Int(4), Cell::Text("no id".into())]);
        table
    }

    fn focus_on(table: &Table, text: &str) -> Option<Focus> {
        resolve_focus(table, &TraceColumns::detect(table), FocusTarget::Id(text))
    }

    #[test]
    fn a_focus_is_an_activity_and_every_row_below_it() {
        let table = tree();
        let focus = focus_on(&table, "a").expect("a focus");
        assert_eq!(focus.activity_id, "a");
        assert_eq!(focus.parent_id.as_deref(), Some("r"));
        assert_eq!(focus.marker.as_deref(), Some("marker a"));
        assert_eq!(focus.rows, vec![1, 2, 4], "a's two events and c's one, in source order");

        let root = focus_on(&table, "r").expect("a focus");
        assert_eq!(root.parent_id, None);
        assert_eq!(root.rows, vec![0, 1, 2, 3, 4, 6]);
        assert_eq!(focus_on(&table, "c").expect("a focus").rows, vec![2]);
    }

    #[test]
    fn an_id_is_matched_exactly_then_ignoring_case_and_padding() {
        let mut table = trace(&[("Abc", "", 4), ("abc", "", 4)]);
        assert_eq!(focus_on(&table, "abc").expect("exact").rows, vec![1]);
        assert_eq!(focus_on(&table, "  Abc ").expect("trimmed").rows, vec![0]);
        assert_eq!(focus_on(&table, "ABC").expect("ignoring case").rows, vec![0], "the first of the two");
        assert!(focus_on(&table, "nothing").is_none());
        assert!(focus_on(&table, "").is_none());
        table.columns.remove(1);
        for row in &mut table.rows {
            row.remove(1);
        }
        assert!(focus_on(&table, "abc").is_none(), "no parent column, no activities");
    }

    #[test]
    fn a_row_focuses_the_activity_it_belongs_to_unless_it_has_none() {
        let table = tree();
        let columns = TraceColumns::detect(&table);
        let by_row = |row| resolve_focus(&table, &columns, FocusTarget::Row(row));
        assert_eq!(by_row(4).expect("a focus").activity_id, "a");
        assert_eq!(by_row(2).expect("a focus").rows, vec![2]);
        assert!(by_row(7).is_none(), "the row with no activity id has nothing to focus on");
        assert!(by_row(99).is_none());
    }

    #[test]
    fn a_projection_of_a_focus_holds_only_its_activities_with_their_source_rows() {
        let table = tree();
        let columns = TraceColumns::detect(&table);
        let focus = focus_on(&table, "a").expect("a focus");
        let projection = build_projection_in(&table, &columns, Some(&focus)).expect("a projection");
        let ids: Vec<&str> = projection.activities.iter().map(|a| a.activity_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        let a = find(&projection, "a");
        assert_eq!(a.parent, None, "the focus is a root");
        assert_eq!(a.issue, None, "and not an orphan, though its parent is outside");
        assert_eq!(a.event_rows, vec![1, 4], "the source rows are kept");
        assert_eq!(find(&projection, "c").parent, Some(0));
        assert_eq!(projection.activities[0].subtree_activity_count, 2);

        let whole = build_projection_in(&table, &columns, None).expect("a projection");
        let orphans = whole.activities.iter().filter(|a| a.issue == Some(HierarchyIssue::Orphan)).count();
        assert_eq!(orphans, 0, "the whole trace is unchanged");
        let root = focus_on(&table, "r").expect("a focus");
        let from_root = build_projection_in(&table, &columns, Some(&root)).expect("a projection");
        assert_eq!(from_root.activities.len(), 4, "r, a, c and b, not z or the row with no id");
    }

    #[test]
    fn a_leaf_is_a_one_activity_trace() {
        let table = tree();
        let columns = TraceColumns::detect(&table);
        let focus = focus_on(&table, "b").expect("a focus");
        let projection = build_projection_in(&table, &columns, Some(&focus)).expect("a projection");
        assert_eq!(projection.activities.len(), 1);
        assert_eq!(projection.activities[0].event_rows, vec![3]);
    }
}
