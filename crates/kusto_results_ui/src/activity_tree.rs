//! The activity tree of the structured view: the activities of a trace as a tree, with the
//! severity of each, and the Deepest action.

use std::sync::Arc;

use gpui::{
    App, ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement, Render,
    Role, ScrollStrategy, SharedString, UniformListScrollHandle, Window, actions, div, px,
    uniform_list,
};
use kusto_results::activity::{ActivityProjection, HierarchyIssue, Strength};
use kusto_results::activity_tree::ActivityTreeState;
use settings::Settings as _;
use ui::{Button, ButtonSize, Icon, IconName, IconSize, Tooltip, prelude::*};

use crate::results_settings::ResultsSettings;

actions!(
    activity_tree,
    [
        /// Selects the activity below.
        SelectNext,
        /// Selects the activity above.
        SelectPrevious,
        /// Opens the selected activity's branch.
        Expand,
        /// Closes the selected activity's branch, or selects its parent when it is closed.
        Collapse,
        /// Shows the selected activity's events.
        Activate,
        /// Selects the next of the deepest activities.
        RevealDeepest,
    ]
);

/// The depth badge and the event count come first, in columns, so the names line up.
const DEPTH_COLUMN_WIDTH: Pixels = px(28.);
const COUNT_COLUMN_WIDTH: Pixels = px(40.);
/// A node's indent for each level.
const INDENT: Pixels = px(16.);
/// The strength of a node's colour when its issue was handled: a warning or error earlier in
/// the activity, then a normal last event.
const HANDLED_ISSUE_STRENGTH: f32 = 0.3;

pub enum ActivityTreeEvent {
    /// The selected activity changed, as an index into the projection's activities.
    SelectionChanged(usize),
}

pub struct ActivityTree {
    state: ActivityTreeState,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
}

impl EventEmitter<ActivityTreeEvent> for ActivityTree {}

impl Focusable for ActivityTree {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn level_name(level: u8) -> &'static str {
    match level {
        1 => "critical",
        2 => "error",
        _ => "warning",
    }
}

impl ActivityTree {
    pub fn new(projection: Arc<ActivityProjection>, cx: &mut Context<Self>) -> Self {
        Self {
            state: ActivityTreeState::new(projection),
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    pub fn selected(&self) -> usize {
        self.state.selected()
    }

    pub fn state(&self) -> &ActivityTreeState {
        &self.state
    }

    /// Selects an activity, telling listeners when that changes it.
    pub fn select(&mut self, activity: usize, cx: &mut Context<Self>) {
        if self.state.select(activity) {
            cx.emit(ActivityTreeEvent::SelectionChanged(activity));
        }
        cx.notify();
    }

    /// Opens the ancestors of an activity, selects it and scrolls it to the middle, for a link
    /// from another view.
    pub fn reveal(&mut self, activity: usize, cx: &mut Context<Self>) {
        if self.state.reveal(activity) {
            cx.emit(ActivityTreeEvent::SelectionChanged(activity));
        }
        self.reveal_selected(ScrollStrategy::Center);
        cx.notify();
    }

    fn reveal_selected(&self, strategy: ScrollStrategy) {
        if let Some(position) = self.state.position_of(self.state.selected()) {
            self.scroll_handle.scroll_to_item(position, strategy);
        }
    }

    /// Selects the next of the deepest activities, opening its ancestors, focusing the tree
    /// and scrolling the activity to the middle.
    pub fn reveal_deepest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.state.selected();
        if let Some(activity) = self.state.reveal_deepest() {
            if activity != before {
                cx.emit(ActivityTreeEvent::SelectionChanged(activity));
            }
            window.focus(&self.focus_handle, cx);
            self.reveal_selected(ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn move_selection(&mut self, lines: isize, cx: &mut Context<Self>) {
        let before = self.state.selected();
        self.state.move_selection(lines);
        self.after_key(before, cx);
    }

    fn after_key(&mut self, before: usize, cx: &mut Context<Self>) {
        let selected = self.state.selected();
        if selected != before {
            cx.emit(ActivityTreeEvent::SelectionChanged(selected));
        }
        self.reveal_selected(ScrollStrategy::Nearest);
        cx.notify();
    }

    fn click_node(&mut self, activity: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        self.select(activity, cx);
        if event.click_count() == 2 {
            self.state.toggle(activity);
            cx.notify();
        }
    }

    /// The colour a node takes from its severity (ACT-9), or none.
    fn tint(&self, activity: usize, cx: &App) -> Option<Hsla> {
        let severity = self.state.projection().activities.get(activity)?.severity?;
        let tint = ResultsSettings::get_global(cx).severity_tint(severity.level)?;
        Some(match severity.strength {
            Strength::Full => tint,
            Strength::Muted => tint.opacity(HANDLED_ISSUE_STRENGTH),
        })
    }

    #[cfg(test)]
    pub(crate) fn tint_for_test(&self, activity: usize, cx: &App) -> Option<Hsla> {
        self.tint(activity, cx)
    }

    fn render_node(&self, position: usize, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let node = *self.state.visible().get(position)?;
        let activity = self.state.projection().activities.get(node.activity)?;
        let index = node.activity;
        let selected = index == self.state.selected();
        let has_children = self.state.has_children(index);
        let expanded = self.state.is_expanded(index);
        let colors = cx.theme().colors();

        let mut tooltip = activity.activity_id.clone();
        if let Some(marker) = &activity.marker_name {
            tooltip.push_str(" — ");
            tooltip.push_str(marker);
        }
        let triangle_sentence = activity
            .shows_warning_triangle()
            .then(|| {
                activity.severity.map(|severity| {
                    let when = match severity.strength {
                        Strength::Full => "Final",
                        Strength::Muted => "Earlier",
                    };
                    format!(
                        "{when} {} event in this activity",
                        level_name(severity.level)
                    )
                })
            })
            .flatten();
        if let Some(sentence) = &triangle_sentence {
            tooltip.push_str(" — ");
            tooltip.push_str(sentence);
        }
        if let Some(issue) = activity.issue {
            tooltip.push('\n');
            tooltip.push_str(match issue {
                HierarchyIssue::Orphan => "Parent not found",
                HierarchyIssue::ConflictingParents => "Conflicting parents",
                HierarchyIssue::Cycle => "Cycle broken here",
            });
        }
        let label: SharedString = format!(
            "{}{} ({})",
            activity
                .marker_name
                .as_ref()
                .map_or_else(String::new, |marker| format!("{marker} ")),
            activity.activity_id,
            activity.event_rows.len()
        )
        .into();
        let event_count = activity.event_rows.len();
        let depth_below = activity.max_descendant_depth;
        let branch_size = activity.subtree_activity_count;

        let disclosure = if has_children {
            IconButton::new(
                ("activity-disclosure", index),
                if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                },
            )
            .icon_size(IconSize::XSmall)
            .tooltip(Tooltip::text(if expanded { "Collapse" } else { "Expand" }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.toggle(index);
                cx.notify();
            }))
            .into_any_element()
        } else {
            div().w(px(20.)).child("•").into_any_element()
        };

        Some(
            h_flex()
                .id(("activity-node", index))
                .debug_selector(|| format!("activity-node-{index}"))
                .role(Role::TreeItem)
                .aria_label(label)
                .aria_level(node.level + 1)
                .aria_selected(selected)
                .when(has_children, |row| row.aria_expanded(expanded))
                .w_full()
                .h(px(26.))
                .pl(INDENT * node.level as f32)
                .pr_2()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .when_some(self.tint(index, cx), |row, tint| row.bg(tint))
                .when(selected, |row| {
                    row.bg(colors.element_selected)
                        .border_l_2()
                        .border_color(colors.border_focused)
                })
                .tooltip(Tooltip::text(tooltip))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.click_node(index, event, cx)
                }))
                .child(disclosure)
                .when(triangle_sentence.is_some(), |row| {
                    row.child(
                        Icon::new(IconName::Warning)
                            .size(IconSize::XSmall)
                            .color(Color::Warning),
                    )
                })
                .child(
                    div()
                        .id(("activity-depth", index))
                        .min_w(DEPTH_COLUMN_WIDTH)
                        .when(has_children, |cell| {
                            cell.tooltip(Tooltip::text(format!(
                                "{depth_below} level{} below; {branch_size} activities in this branch",
                                if depth_below == 1 { "" } else { "s" }
                            )))
                            .child(
                                Label::new(format!("↓{depth_below}"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .child(
                    div().min_w(COUNT_COLUMN_WIDTH).child(
                        Label::new(format!("({event_count})"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .when_some(activity.marker_name.clone(), |row, marker| {
                    row.child(Label::new(marker).single_line())
                })
                .child(
                    Label::new(activity.activity_id.clone())
                        .single_line()
                        .color(if activity.marker_name.is_some() {
                            Color::Muted
                        } else {
                            Color::Default
                        }),
                )
                .into_any_element(),
        )
    }
}

impl Render for ActivityTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.state.visible().len();
        let deepest = self.state.deepest_summary();
        let colors = cx.theme().colors();
        v_flex()
            .id("activity-tree")
            .key_context("ActivityTree")
            .track_focus(&self.focus_handle)
            .role(Role::Tree)
            .aria_label("Activities")
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Expand, _, cx| {
                let before = this.state.selected();
                this.state.key_right();
                this.after_key(before, cx);
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| {
                let before = this.state.selected();
                this.state.key_left();
                this.after_key(before, cx);
            }))
            .on_action(cx.listener(|this, _: &Activate, _, cx| {
                let selected = this.state.selected();
                cx.emit(ActivityTreeEvent::SelectionChanged(selected));
            }))
            .on_action(
                cx.listener(|this, _: &RevealDeepest, window, cx| this.reveal_deepest(window, cx)),
            )
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .justify_between()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(Label::new("Activities").weight(gpui::FontWeight::SEMIBOLD))
                    .when_some(deepest, |header, (depth, tied)| {
                        header.child(
                            div()
                                .debug_selector(|| "activity-deepest".to_string())
                                .child(
                                    Button::new("activity-deepest", format!("Deepest · {depth}"))
                                        .size(ButtonSize::Compact)
                                        .tooltip(Tooltip::text(format!(
                                            "Reveal deepest activity ({tied} at level {depth})"
                                        )))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.reveal_deepest(window, cx)
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                div().flex_1().min_h_0().child(
                    uniform_list(
                        "activity-tree-nodes",
                        count,
                        cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                            range
                                .filter_map(|position| this.render_node(position, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll_handle)
                    .size_full(),
                ),
            )
    }
}
