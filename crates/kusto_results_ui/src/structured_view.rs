//! The structured view of a trace: the activity tree on the left and, on the right, the events
//! of the selected activity in a results grid, with a splitter between them.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Bounds, Context, DragMoveEvent, Empty, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Orientation, Pixels, Render, Role, Subscription, Window, actions, canvas, div, px,
};
use kusto_results::ResultSet;
use kusto_results::activity::ActivityProjection;
use kusto_results::timeline::Timeline;
use ui::prelude::*;

use crate::activity_tree::{ActivityTree, ActivityTreeEvent};
use crate::grid::{GridOptions, ResultGrid};

actions!(
    structured_splitter,
    [
        /// Moves the splitter left a little.
        MoveLeft,
        /// Moves the splitter right a little.
        MoveRight,
        /// Moves the splitter left a lot.
        MoveLeftLarge,
        /// Moves the splitter right a lot.
        MoveRightLarge,
        /// Moves the splitter as far left as it goes.
        MoveToStart,
        /// Moves the splitter as far right as it goes.
        MoveToEnd,
    ]
);

const DEFAULT_TREE_WIDTH: Pixels = px(340.);
const MINIMUM_TREE_WIDTH: Pixels = px(180.);
const MINIMUM_EVENTS_WIDTH: Pixels = px(280.);
const SPLITTER_WIDTH: Pixels = px(6.);
const SMALL_STEP: Pixels = px(20.);
const LARGE_STEP: Pixels = px(80.);

#[derive(Clone)]
struct SplitterDrag;

impl Render for SplitterDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub enum StructuredViewEvent {
    /// The user asked, from the tree's context menu, to focus on the activity with this id.
    FocusRequested(String),
}

impl EventEmitter<StructuredViewEvent> for StructuredView {}

pub struct StructuredView {
    tree: Entity<ActivityTree>,
    grid: Entity<ResultGrid>,
    projection: Arc<ActivityProjection>,
    tree_width: Pixels,
    /// Where the view was last laid out, to keep the splitter inside it.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    splitter_focus: FocusHandle,
    _subscription: Subscription,
}

impl StructuredView {
    pub fn new(
        result: Arc<ResultSet>,
        table_index: usize,
        projection: Arc<ActivityProjection>,
        timeline: Option<Arc<Timeline>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree = cx.new(|cx| ActivityTree::new(projection.clone(), timeline, cx));
        let first = tree.read(cx).selected();
        let table_name = result
            .tables
            .get(table_index)
            .map_or_else(String::new, |table| table.name.clone());
        let grid = cx.new(|cx| {
            ResultGrid::with_options(
                result,
                table_index,
                GridOptions {
                    view_name: Some(format!("{table_name}::activity-structured:{table_index}")),
                    scope: Some(Arc::new(
                        projection
                            .activities
                            .get(first)
                            .map(|activity| activity.event_rows.clone())
                            .unwrap_or_default(),
                    )),
                },
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&tree, |this, _, event: &ActivityTreeEvent, cx| match event {
            ActivityTreeEvent::SelectionChanged(activity) => this.show_events_of(*activity, cx),
            ActivityTreeEvent::FocusRequested(id) => {
                cx.emit(StructuredViewEvent::FocusRequested(id.clone()))
            }
        });
        Self {
            tree,
            grid,
            projection,
            tree_width: DEFAULT_TREE_WIDTH,
            bounds: Rc::new(Cell::new(Bounds::default())),
            splitter_focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn tree(&self) -> &Entity<ActivityTree> {
        &self.tree
    }

    pub fn grid(&self) -> &Entity<ResultGrid> {
        &self.grid
    }

    pub fn tree_width(&self) -> Pixels {
        self.tree_width
    }

    fn show_events_of(&mut self, activity: usize, cx: &mut Context<Self>) {
        let rows = self
            .projection
            .activities
            .get(activity)
            .map(|activity| activity.event_rows.clone())
            .unwrap_or_default();
        self.grid
            .update(cx, |grid, cx| grid.set_scope(Some(Arc::new(rows)), cx));
        cx.notify();
    }

    /// The widest the tree may be: the events pane keeps its minimum. Before the first layout
    /// there is nothing to limit it by.
    fn maximum_tree_width(&self) -> Pixels {
        let width = self.bounds.get().size.width;
        if width <= px(0.) {
            return px(10_000.);
        }
        (width - MINIMUM_EVENTS_WIDTH - SPLITTER_WIDTH).max(MINIMUM_TREE_WIDTH)
    }

    fn set_tree_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        self.tree_width = width.clamp(MINIMUM_TREE_WIDTH, self.maximum_tree_width());
        cx.notify();
    }

    fn nudge(&mut self, by: Pixels, cx: &mut Context<Self>) {
        self.set_tree_width(self.tree_width + by, cx);
    }

    fn events_heading(&self, cx: &App) -> String {
        let selected = self.tree.read(cx).selected();
        match self.projection.activities.get(selected) {
            Some(activity) => format!(
                "Events of the selected activity ({})",
                activity
                    .marker_name
                    .as_deref()
                    .unwrap_or(&activity.activity_id)
            ),
            None => "Events".to_string(),
        }
    }
}

impl Focusable for StructuredView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tree.read(cx).focus_handle(cx)
    }
}

impl Render for StructuredView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let minimum = MINIMUM_TREE_WIDTH;
        let maximum = self.maximum_tree_width().max(minimum);
        let heading = self.events_heading(cx);
        let bounds = self.bounds.clone();
        h_flex()
            .size_full()
            .relative()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<SplitterDrag>, _, cx| {
                    this.set_tree_width(event.event.position.x - event.bounds.left(), cx)
                }),
            )
            .child(
                div()
                    .w(self.tree_width)
                    .h_full()
                    .flex_none()
                    .child(self.tree.clone()),
            )
            .child(
                div()
                    .id("structured-splitter")
                    .debug_selector(|| "structured-splitter".to_string())
                    .key_context("StructuredSplitter")
                    .track_focus(&self.splitter_focus)
                    .role(Role::Splitter)
                    .aria_label("Resize the activity tree")
                    .aria_orientation(Orientation::Vertical)
                    .aria_numeric_value(f32::from(self.tree_width) as f64)
                    .aria_min_numeric_value(f32::from(minimum) as f64)
                    .aria_max_numeric_value(f32::from(maximum) as f64)
                    .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.nudge(-SMALL_STEP, cx)))
                    .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.nudge(SMALL_STEP, cx)))
                    .on_action(
                        cx.listener(|this, _: &MoveLeftLarge, _, cx| this.nudge(-LARGE_STEP, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &MoveRightLarge, _, cx| this.nudge(LARGE_STEP, cx)),
                    )
                    .on_action(cx.listener(|this, _: &MoveToStart, _, cx| {
                        this.set_tree_width(MINIMUM_TREE_WIDTH, cx)
                    }))
                    .on_action(cx.listener(|this, _: &MoveToEnd, _, cx| {
                        let widest = this.maximum_tree_width();
                        this.set_tree_width(widest, cx)
                    }))
                    .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                        window.focus(&this.splitter_focus, cx);
                        if event.click_count() == 2 {
                            this.set_tree_width(DEFAULT_TREE_WIDTH, cx);
                        }
                    }))
                    .h_full()
                    .w(SPLITTER_WIDTH)
                    .flex_none()
                    .cursor_col_resize()
                    .bg(colors.border)
                    .hover(|style| style.bg(colors.border_focused))
                    .on_drag(SplitterDrag, |drag, _, _, cx| cx.new(|_| drag.clone())),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(MINIMUM_EVENTS_WIDTH)
                    .h_full()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.border)
                            .child(Label::new(heading).weight(gpui::FontWeight::SEMIBOLD)),
                    )
                    .child(div().flex_1().min_h_0().child(self.grid.clone())),
            )
            .child(
                canvas(move |measured, _, _| bounds.set(measured), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Point, TestAppContext,
        VisualTestContext, size,
    };
    use kusto_results::activity::build_projection;
    use kusto_results::{Cell, Column, Table};

    use super::*;
    use crate::activity_tree::{Collapse, Expand, SelectNext};

    /// R has two events, children A (three) and B (one), and C (two) below A.
    fn trace() -> Arc<ResultSet> {
        let rows: &[(&str, &str, i64, &str, &str)] = &[
            ("r1", "Request", 4, "R", ""),
            ("a1", "Auth", 4, "A", "R"),
            ("a2", "Auth", 3, "A", "R"),
            ("c1", "Retry", 2, "C", "A"),
            ("b1", "Notify", 4, "B", "R"),
            ("r2", "Request", 4, "R", ""),
            ("a3", "Auth", 4, "A", "R"),
            ("c2", "Retry", 4, "C", "A"),
        ];
        Arc::new(ResultSet {
            tables: vec![Table {
                name: "t".into(),
                columns: vec![
                    Column::new("MessageText", "string"),
                    Column::new("MarkerName", "string"),
                    Column::new("Severity", "long"),
                    Column::new("CurrentActivityId", "string"),
                    Column::new("ParentActivityId", "string"),
                ],
                rows: rows
                    .iter()
                    .map(|(text, marker, level, current, parent)| {
                        vec![
                            Cell::Text((*text).into()),
                            Cell::Text((*marker).into()),
                            Cell::Int(*level),
                            Cell::Text((*current).into()),
                            if parent.is_empty() {
                                Cell::Null
                            } else {
                                Cell::Text((*parent).into())
                            },
                        ]
                    })
                    .collect(),
            }],
            ..Default::default()
        })
    }

    fn open(cx: &mut TestAppContext) -> (Entity<StructuredView>, &mut VisualTestContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let result = trace();
        let projection = Arc::new(build_projection(&result.tables[0]).expect("activity columns"));
        let (view, cx) =
            cx.add_window_view(|window, cx| StructuredView::new(result, 0, projection, None, window, cx));
        cx.simulate_resize(size(px(1000.), px(600.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn grid_rows(view: &Entity<StructuredView>, cx: &mut VisualTestContext) -> Vec<usize> {
        view.read_with(cx, |view, cx| {
            view.grid().read(cx).visible_rows_for_test().to_vec()
        })
    }

    fn activity_of(view: &Entity<StructuredView>, name: &str, cx: &mut VisualTestContext) -> usize {
        view.read_with(cx, |view, _| {
            view.projection
                .activities
                .iter()
                .position(|activity| activity.activity_id == name)
                .unwrap_or_else(|| panic!("no activity {name}"))
        })
    }

    /// ACT-6, ACT-13: the grid shows only the selected activity's events, source row numbers
    /// and all, and choosing another activity clears the selection.
    #[gpui::test]
    async fn the_grid_shows_the_events_of_the_selected_activity(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        assert_eq!(grid_rows(&view, cx), [0, 5], "the first root's own events");
        assert_eq!(
            view.read_with(cx, |view, cx| view.grid().read(cx).status_text()),
            "2 rows"
        );

        let auth = activity_of(&view, "A", cx);
        let tree = view.read_with(cx, |view, _| view.tree().clone());
        tree.update(cx, |tree, cx| tree.select(auth, cx));
        cx.run_until_parked();
        assert_eq!(grid_rows(&view, cx), [1, 2, 6]);

        let active_rows =
            cx.update(|_, cx| crate::ActiveSelection::shared(cx).read(cx).rows.clone());
        assert!(active_rows.is_empty(), "an empty selection is published");
        let layout_name = cx.update(|window, cx| {
            view.read(cx)
                .grid()
                .read(cx)
                .layout(window, cx)
                .map(|layout| layout.name)
        });
        assert_eq!(layout_name.as_deref(), Some("t::activity-structured:0"));
    }

    /// ACT-11: Deepest reveals the deepest activity, opening its ancestors.
    #[gpui::test]
    async fn deepest_reveals_and_shows_the_deepest_activity(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let tree = view.read_with(cx, |view, _| view.tree().clone());
        assert_eq!(
            tree.read_with(cx, |tree, _| tree.state().visible().len()),
            1,
            "starts with only the root showing"
        );
        assert!(cx.debug_bounds("activity-deepest").is_some());

        let point = cx
            .debug_bounds("activity-deepest")
            .map(|bounds| bounds.center())
            .expect("the button shows");
        cx.simulate_click(point, Modifiers::default());
        cx.run_until_parked();
        draw(cx);
        let retry = activity_of(&view, "C", cx);
        assert_eq!(tree.read_with(cx, |tree, _| tree.selected()), retry);
        assert_eq!(grid_rows(&view, cx), [3, 7]);
        assert_eq!(
            tree.read_with(cx, |tree, _| tree.state().visible().len()),
            4,
            "R, A, C and B are showing"
        );
    }

    /// ACT-8: the keyboard walks and opens the tree.
    #[gpui::test]
    async fn the_keyboard_walks_the_tree(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let tree = view.read_with(cx, |view, _| view.tree().clone());
        let focus = tree.read_with(cx, |tree, cx| tree.focus_handle(cx));
        cx.update(|window, cx| window.focus(&focus, cx));

        cx.dispatch_action(Expand);
        assert_eq!(
            tree.read_with(cx, |tree, _| tree.state().visible().len()),
            3
        );
        cx.dispatch_action(SelectNext);
        cx.run_until_parked();
        assert_eq!(grid_rows(&view, cx), [1, 2, 6], "the first child");
        cx.dispatch_action(Collapse);
        cx.dispatch_action(Collapse);
        cx.run_until_parked();
        assert_eq!(grid_rows(&view, cx), [0, 5], "back to the root");
    }

    fn double_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
        for click_count in 1..=2 {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers: Modifiers::default(),
                click_count,
            });
        }
    }

    /// ACT-12: the splitter has a default, limits, key steps, a drag and a double-click reset.
    #[gpui::test]
    async fn the_splitter_moves_within_its_limits(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let width = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.tree_width());
        assert_eq!(width(cx), px(340.));

        let focus = view.read_with(cx, |view, _| view.splitter_focus.clone());
        cx.update(|window, cx| window.focus(&focus, cx));
        cx.dispatch_action(MoveLeft);
        assert_eq!(width(cx), px(320.));
        cx.dispatch_action(MoveRightLarge);
        assert_eq!(width(cx), px(400.));
        cx.dispatch_action(MoveToStart);
        assert_eq!(width(cx), px(180.));
        cx.dispatch_action(MoveLeftLarge);
        assert_eq!(width(cx), px(180.), "the tree keeps its minimum");
        cx.dispatch_action(MoveToEnd);
        draw(cx);
        assert_eq!(
            width(cx),
            px(1000.) - px(280.) - SPLITTER_WIDTH,
            "the events pane keeps its minimum"
        );
        cx.dispatch_action(MoveRightLarge);
        assert_eq!(width(cx), px(1000.) - px(280.) - SPLITTER_WIDTH);

        let handle = cx
            .debug_bounds("structured-splitter")
            .map(|bounds| bounds.center())
            .expect("the splitter shows");
        double_click(cx, handle);
        assert_eq!(width(cx), px(340.), "double-click resets it");

        let handle = cx
            .debug_bounds("structured-splitter")
            .map(|bounds| bounds.center())
            .expect("the splitter shows");
        cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        for step in 1..=6 {
            cx.simulate_mouse_move(
                Point {
                    x: handle.x + px(100.) * (step as f32 / 6.),
                    y: handle.y,
                },
                MouseButton::Left,
                Modifiers::default(),
            );
        }
        cx.simulate_mouse_up(
            Point {
                x: handle.x + px(100.),
                y: handle.y,
            },
            MouseButton::Left,
            Modifiers::default(),
        );
        assert!(width(cx) > px(420.), "the drag widened it: {:?}", width(cx));
    }

    /// ACT-9, ACT-10: a node takes its last event's colour at full strength, or the worst earlier
    /// issue's at 30 % when the activity recovered, and only an activity with its own warning,
    /// error or critical event gets the triangle.
    #[gpui::test]
    async fn nodes_are_coloured_by_severity_and_marked_for_their_own_issues(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = open(cx);
        let tree = view.read_with(cx, |view, _| view.tree().clone());
        let request = activity_of(&view, "R", cx);
        let auth = activity_of(&view, "A", cx);
        let retry = activity_of(&view, "C", cx);
        let tint = |activity: usize, cx: &mut VisualTestContext| {
            cx.update(|_, cx| tree.read(cx).tint_for_test(activity, cx))
        };
        let alpha = |colour: Option<gpui::Hsla>| colour.map(|colour| colour.a);

        let normal = alpha(tint(request, cx)).expect("normal events are tinted too");
        assert!((normal - 0x1f as f32 / 255.).abs() < 0.001, "{normal}");
        let handled_error = alpha(tint(retry, cx)).expect("an earlier error leaves a mark");
        assert!(
            (handled_error - 0x33 as f32 / 255. * 0.3).abs() < 0.001,
            "{handled_error}"
        );
        let handled_warning = alpha(tint(auth, cx)).expect("an earlier warning leaves a mark");
        assert!(
            (handled_warning - 0x2e as f32 / 255. * 0.3).abs() < 0.001,
            "{handled_warning}"
        );

        view.read_with(cx, |view, _| {
            let activities = &view.projection.activities;
            assert!(!activities[request].shows_warning_triangle());
            assert!(activities[auth].shows_warning_triangle());
            assert!(activities[retry].shows_warning_triangle());
        });
    }

    /// A trace with timestamps shows how long each activity ran and when it started.
    #[gpui::test]
    async fn the_tree_shows_how_long_each_activity_ran_and_when_it_started(
        cx: &mut TestAppContext,
    ) {
        use kusto_results::activity::build_projection_with;
        use kusto_results::timeline::{Timeline, TimelineOptions};
        use kusto_results::trace_schema::TraceColumns;

        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let events: &[(&str, &str, u32)] = &[("r", "", 0), ("r", "", 100), ("a", "r", 10), ("a", "r", 40)];
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("CurrentActivityId", "string"),
                Column::new("ParentActivityId", "string"),
                Column::new("TIMESTAMP", "datetime"),
            ],
            rows: events
                .iter()
                .map(|(current, parent, millis)| {
                    vec![
                        Cell::Text((*current).into()),
                        if parent.is_empty() { Cell::Null } else { Cell::Text((*parent).into()) },
                        Cell::Text(format!("2026-01-01T00:00:00.{:07}Z", millis * 10_000)),
                    ]
                })
                .collect(),
        };
        let columns = TraceColumns::detect(&table);
        let projection = build_projection_with(&table, &columns).expect("a projection");
        let timeline = Arc::new(Timeline::build(&table, &projection, &columns, &TimelineOptions::default()));
        let result = Arc::new(ResultSet {
            tables: vec![table],
            ..Default::default()
        });
        let projection = Arc::new(projection);
        let (view, cx) = cx.add_window_view(move |window, cx| {
            StructuredView::new(result, 0, projection, Some(timeline), window, cx)
        });
        let tree = view.read_with(cx, |view, _| view.tree().clone());

        let root = tree.read_with(cx, |tree, _| tree.time_labels_for_test(0)).expect("times for the root");
        assert_eq!((root.duration.as_str(), root.start.as_str()), ("100 ms", "+0.000 s"));
        assert!(root.tooltip.contains("Start 2026-01-01 00:00:00.000 UTC (+0.000 s from the start of the trace)"), "{}", root.tooltip);
        assert!(root.tooltip.contains("Duration 100 ms, of which 70 ms is not covered by traced work"), "{}", root.tooltip);
        let child = tree.read_with(cx, |tree, _| tree.time_labels_for_test(1)).expect("times for the child");
        assert_eq!((child.duration.as_str(), child.start.as_str()), ("30 ms", "+0.010 s"));
        assert!(child.tooltip.contains("Start 2026-01-01 00:00:00.010 UTC"), "{}", child.tooltip);
        assert!(!child.tooltip.contains("not covered"), "a leaf has no children to cover it: {}", child.tooltip);

        let (plain, cx) = open(cx);
        let plain_tree = plain.read_with(cx, |view, _| view.tree().clone());
        assert_eq!(plain_tree.read_with(cx, |tree, _| tree.time_labels_for_test(0)), None, "no timestamps, no times");
    }
}
