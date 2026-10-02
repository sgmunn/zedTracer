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
    let current_column = columns.activity_id?;
    let parent_column = columns.parent_activity_id?;
    let severity_column = columns.severity;
    let marker_column = columns.marker;

    let mut groups: Vec<Group> = Vec::new();
    let mut group_by_id: HashMap<String, usize> = HashMap::new();
    for (row_index, row) in table.rows.iter().enumerate() {
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
}
