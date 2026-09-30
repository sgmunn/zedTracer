//! The state of the activity tree: which branches are open, which activity is selected, and
//! how the keyboard and the Deepest action move around.
//!
//! Nothing here draws anything. The tree view asks this state what to show.

use std::collections::HashSet;
use std::sync::Arc;

use crate::activity::ActivityProjection;

/// One line of the tree as drawn: an activity and how far in it is indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleNode {
    pub activity: usize,
    /// The activity's tree level, 0 for a root.
    pub level: usize,
}

pub struct ActivityTreeState {
    projection: Arc<ActivityProjection>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    expanded: HashSet<usize>,
    selected: usize,
    /// Which of the deepest activities the next use of Deepest reveals.
    deepest_position: usize,
    visible: Vec<VisibleNode>,
}

impl ActivityTreeState {
    /// Every branch collapsed and the first root selected.
    pub fn new(projection: Arc<ActivityProjection>) -> Self {
        let mut children = vec![Vec::new(); projection.activities.len()];
        let mut roots = Vec::new();
        for (index, activity) in projection.activities.iter().enumerate() {
            match activity.parent {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }
        let selected = roots.first().copied().unwrap_or(0);
        let mut state = Self {
            projection,
            children,
            roots,
            expanded: HashSet::new(),
            selected,
            deepest_position: 0,
            visible: Vec::new(),
        };
        state.rebuild_visible();
        state
    }

    pub fn projection(&self) -> &Arc<ActivityProjection> {
        &self.projection
    }

    pub fn visible(&self) -> &[VisibleNode] {
        &self.visible
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_expanded(&self, activity: usize) -> bool {
        self.expanded.contains(&activity)
    }

    pub fn has_children(&self, activity: usize) -> bool {
        self.children
            .get(activity)
            .is_some_and(|children| !children.is_empty())
    }

    /// The line an activity is on, when its ancestors are open.
    pub fn position_of(&self, activity: usize) -> Option<usize> {
        self.visible
            .iter()
            .position(|node| node.activity == activity)
    }

    pub fn select(&mut self, activity: usize) -> bool {
        if activity >= self.projection.activities.len() || activity == self.selected {
            return false;
        }
        self.selected = activity;
        true
    }

    pub fn toggle(&mut self, activity: usize) {
        if self.expanded.contains(&activity) {
            self.collapse(activity);
        } else {
            self.expand(activity);
        }
    }

    /// Opens a branch. Returns whether anything changed.
    pub fn expand(&mut self, activity: usize) -> bool {
        if !self.has_children(activity) || !self.expanded.insert(activity) {
            return false;
        }
        self.rebuild_visible();
        true
    }

    /// Closes a branch. A selection inside it moves to the branch itself, so the selected
    /// activity is never hidden. Returns whether anything changed.
    pub fn collapse(&mut self, activity: usize) -> bool {
        if !self.expanded.remove(&activity) {
            return false;
        }
        if self.is_inside(self.selected, activity) {
            self.selected = activity;
        }
        self.rebuild_visible();
        true
    }

    /// Right on a node: opens it.
    pub fn key_right(&mut self) -> bool {
        self.expand(self.selected)
    }

    /// Left on a node: closes it, or when it is already closed selects its parent.
    pub fn key_left(&mut self) -> bool {
        if self.collapse(self.selected) {
            return true;
        }
        match self
            .projection
            .activities
            .get(self.selected)
            .and_then(|activity| activity.parent)
        {
            Some(parent) => self.select(parent),
            None => false,
        }
    }

    /// Moves the selection to the line above or below.
    pub fn move_selection(&mut self, lines: isize) -> bool {
        let Some(position) = self.position_of(self.selected) else {
            return false;
        };
        let target = (position as isize + lines).clamp(0, self.visible.len() as isize - 1) as usize;
        match self.visible.get(target).map(|node| node.activity) {
            Some(activity) => self.select(activity),
            None => false,
        }
    }

    /// Deepest: the next of the deepest activities, which is opened to, selected and returned.
    /// Repeated use cycles through the ones tied for depth.
    pub fn reveal_deepest(&mut self) -> Option<usize> {
        let (_, tied) = self.projection.deepest();
        let activity = *tied.get(self.deepest_position % tied.len().max(1))?;
        self.deepest_position = (self.deepest_position + 1) % tied.len();
        for ancestor in self.projection.ancestors(activity) {
            self.expanded.insert(ancestor);
        }
        self.selected = activity;
        self.rebuild_visible();
        Some(activity)
    }

    /// The Deepest button's label and tooltip numbers: the depth, and how many activities
    /// share it. `None` when every activity is a root, so there is nothing to reveal.
    pub fn deepest_summary(&self) -> Option<(usize, usize)> {
        let (depth, tied) = self.projection.deepest();
        (depth > 0).then_some((depth, tied.len()))
    }

    fn is_inside(&self, activity: usize, branch: usize) -> bool {
        activity != branch && self.projection.ancestors(activity).contains(&branch)
    }

    fn rebuild_visible(&mut self) {
        let mut visible = Vec::new();
        // Depth-first with an explicit stack: a chain can be tens of thousands deep.
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(activity) = stack.pop() {
            visible.push(VisibleNode {
                activity,
                level: self.projection.activities[activity].depth,
            });
            if self.expanded.contains(&activity) {
                stack.extend(self.children[activity].iter().rev().copied());
            }
        }
        self.visible = visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::build_projection;
    use crate::result::{Cell, Column, Table};

    /// Activities are named in first-seen order: a is the root, b and e are its children, c is
    /// below b, and d below c, which makes d the deepest.
    fn sample() -> ActivityTreeState {
        let rows: &[(&str, &str)] = &[
            ("a", ""),
            ("b", "a"),
            ("c", "b"),
            ("d", "c"),
            ("e", "a"),
            ("f", ""),
        ];
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
            ],
            rows: rows
                .iter()
                .map(|(current, parent)| {
                    vec![Cell::Text((*current).into()), Cell::Text((*parent).into())]
                })
                .collect(),
        };
        ActivityTreeState::new(Arc::new(
            build_projection(&table).expect("activity columns"),
        ))
    }

    fn shown(state: &ActivityTreeState) -> Vec<(String, usize)> {
        state
            .visible()
            .iter()
            .map(|node| {
                (
                    state.projection().activities[node.activity]
                        .activity_id
                        .clone(),
                    node.level,
                )
            })
            .collect()
    }

    fn id(state: &ActivityTreeState, activity: usize) -> &str {
        &state.projection().activities[activity].activity_id
    }

    #[test]
    fn it_starts_collapsed_with_the_first_root_selected() {
        let state = sample();
        assert_eq!(shown(&state), [("a".into(), 0), ("f".into(), 0)]);
        assert_eq!(id(&state, state.selected()), "a");
        assert!(state.has_children(0));
        assert!(!state.is_expanded(0));
    }

    #[test]
    fn opening_a_branch_shows_its_children_in_order() {
        let mut state = sample();
        assert!(state.expand(0));
        assert!(!state.expand(0), "already open");
        assert!(!state.expand(3), "a leaf has nothing to open");
        assert_eq!(
            shown(&state),
            [
                ("a".into(), 0),
                ("b".into(), 1),
                ("e".into(), 1),
                ("f".into(), 0)
            ]
        );
    }

    #[test]
    fn closing_a_branch_moves_a_hidden_selection_to_it() {
        let mut state = sample();
        state.reveal_deepest();
        assert_eq!(id(&state, state.selected()), "d");
        assert!(state.collapse(0));
        assert_eq!(id(&state, state.selected()), "a", "d is hidden, a is shown");
        assert_eq!(shown(&state).len(), 2);
    }

    #[test]
    fn right_opens_and_left_closes_or_goes_to_the_parent() {
        let mut state = sample();
        assert!(state.key_right());
        assert!(!state.key_right(), "nothing more to open");
        assert!(state.move_selection(1));
        assert_eq!(id(&state, state.selected()), "b");
        assert!(state.key_left(), "b is closed, so Left goes to a");
        assert_eq!(id(&state, state.selected()), "a");
        assert!(state.key_left(), "a is open, so Left closes it");
        assert!(!state.is_expanded(0));
        assert!(!state.key_left(), "a root that is closed has nowhere to go");
    }

    #[test]
    fn up_and_down_move_over_the_lines_that_show() {
        let mut state = sample();
        state.expand(0);
        assert!(state.move_selection(1));
        assert!(state.move_selection(1));
        assert!(state.move_selection(1));
        assert_eq!(id(&state, state.selected()), "f");
        assert!(!state.move_selection(1), "the last line");
        assert!(state.move_selection(-10));
        assert_eq!(id(&state, state.selected()), "a");
    }

    #[test]
    fn deepest_opens_the_ancestors_and_cycles_through_ties() {
        let mut state = sample();
        assert_eq!(state.deepest_summary(), Some((3, 1)));
        let first = state.reveal_deepest();
        assert_eq!(first.map(|found| id(&state, found)), Some("d"));
        assert_eq!(
            shown(&state)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e", "f"],
            "every ancestor of d is open"
        );
        assert_eq!(
            state.reveal_deepest(),
            first,
            "a single deepest activity repeats"
        );
    }

    #[test]
    fn a_table_of_roots_has_no_deepest_to_reveal() {
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
            ],
            rows: vec![vec![Cell::Text("x".into()), Cell::Null]],
        };
        let state = ActivityTreeState::new(Arc::new(
            build_projection(&table).expect("activity columns"),
        ));
        assert_eq!(state.deepest_summary(), None);
    }

    #[test]
    fn a_very_deep_chain_does_not_overflow_the_stack() {
        let depth = 20_000;
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
            ],
            rows: (0..depth)
                .map(|index| {
                    vec![
                        Cell::Text(format!("a{index}")),
                        if index == 0 {
                            Cell::Null
                        } else {
                            Cell::Text(format!("a{}", index - 1))
                        },
                    ]
                })
                .collect(),
        };
        let mut state = ActivityTreeState::new(Arc::new(
            build_projection(&table).expect("activity columns"),
        ));
        state.reveal_deepest();
        assert_eq!(state.visible().len(), depth);
    }
}
