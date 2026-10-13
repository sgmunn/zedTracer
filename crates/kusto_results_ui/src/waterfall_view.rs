//! The Timeline tab: a trace as rows of bars on a shared time axis, where the time went (WFL).
//!
//! Bars are placed with fractions of the row's width, so nothing here needs to measure the
//! window: zooming changes which part of the trace the fractions span.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, Bounds, ClickEvent, Context, DismissEvent, DragMoveEvent, Empty,
    Entity, EventEmitter, Focusable, Hsla, IntoElement,
    MouseButton, MouseDownEvent, Point, Render, ScrollStrategy, ScrollWheelEvent, SharedString,
    Subscription, UniformListScrollHandle, Window, anchored, canvas, deferred, div, hsla,
    pattern_slash, px, relative, uniform_list,
};
use gpui_util::ResultExt as _;
use kusto_results::ResultSet;
use kusto_results::activity::ActivityProjection;
use kusto_results::trace_text::{offset_from_start, short_duration, short_marker};
use kusto_results::waterfall::{Folds, Row, Waterfall};
use ui::{Button, ButtonSize, ContextMenu, IconButton, IconName, IconSize, Tooltip, prelude::*};

use crate::row_details_panel::ActiveSelection;

const ROW_HEIGHT: Pixels = px(24.);
const DEFAULT_LABEL_WIDTH: Pixels = px(360.);
const MINIMUM_LABEL_WIDTH: Pixels = px(140.);
/// The bars keep at least this much room, so the splitter cannot push them out of view.
const MINIMUM_BARS_WIDTH: Pixels = px(200.);
const SPLITTER_WIDTH: Pixels = px(6.);
const INDENT: Pixels = px(14.);
const BAR_HEIGHT: Pixels = px(14.);
/// A bar is never thinner than this, so an instant can still be seen and clicked.
const MINIMUM_BAR_WIDTH: Pixels = px(2.);
/// The stripes that mark time the trace does not explain.
const HATCH_WIDTH: f32 = 1.0;
const HATCH_INTERVAL: f32 = 4.0;
/// The part of the trace in view never gets smaller than this share of it.
const SMALLEST_VIEW_SHARE: i64 = 4000;
const ZOOM_STEP: f32 = 1.25;
const AXIS_TICKS: i64 = 8;
/// The hues, in degrees, that actors take in turn, no two closer than 24 degrees and in an order
/// that keeps the first ones far apart.
const ACTOR_HUES: [f32; 10] = [210., 28., 285., 135., 350., 175., 52., 320., 240., 85.];

pub(crate) enum WaterfallEvent {
    /// A row was double-clicked: show this activity's events.
    OpenActivity(usize),
    /// The user asked, from the context menu, to focus on the activity with this id.
    FocusRequested(String),
}

impl EventEmitter<WaterfallEvent> for WaterfallView {}

/// The splitter between the labels and the bars being dragged. It is a type of its own so that a
/// drag here does not move the splitter of a Structured view in the same window.
#[derive(Clone)]
struct LabelSplitterDrag;

impl Render for LabelSplitterDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub(crate) struct WaterfallView {
    result: Arc<ResultSet>,
    projection: Arc<ActivityProjection>,
    waterfall: Arc<Waterfall>,
    folds: Folds,
    /// Open every fold and ignore collapsed rows, so nothing is hidden.
    show_all: bool,
    rows: Vec<Row>,
    selected: Option<usize>,
    /// The part of the trace in view, in ticks from its start.
    view_start: i64,
    view_span: i64,
    highlight_critical: bool,
    label_width: Pixels,
    /// Where the rows and the axis were last laid out, to keep the splitter inside them.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll_handle: UniformListScrollHandle,
    context_menu: Option<(Entity<ContextMenu>, Point<Pixels>, Subscription)>,
}

impl WaterfallView {
    pub(crate) fn new(
        result: Arc<ResultSet>,
        projection: Arc<ActivityProjection>,
        waterfall: Arc<Waterfall>,
    ) -> Self {
        let mut view = Self {
            result,
            projection,
            view_span: waterfall.extent.max(1),
            view_start: 0,
            waterfall,
            folds: Folds::default(),
            show_all: false,
            rows: Vec::new(),
            selected: None,
            highlight_critical: false,
            label_width: DEFAULT_LABEL_WIDTH,
            bounds: Rc::new(Cell::new(Bounds::default())),
            scroll_handle: UniformListScrollHandle::new(),
            context_menu: None,
        };
        view.recompute_rows();
        view
    }

    #[cfg(test)]
    pub(crate) fn label_width(&self) -> Pixels {
        self.shown_label_width()
    }

    /// The widest the labels may be: the bars keep their minimum. Before the first layout there
    /// is nothing to limit it by.
    fn maximum_label_width(&self) -> Pixels {
        let width = self.bounds.get().size.width;
        if width <= px(0.) {
            return px(10_000.);
        }
        (width - MINIMUM_BARS_WIDTH).max(MINIMUM_LABEL_WIDTH)
    }

    /// What was asked for, held inside the room there is now, so a window that has become
    /// narrower does not squeeze the bars away and growing it again gives the width back.
    fn shown_label_width(&self) -> Pixels {
        self.label_width.clamp(MINIMUM_LABEL_WIDTH, self.maximum_label_width())
    }

    fn set_label_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        self.label_width = width.clamp(MINIMUM_LABEL_WIDTH, self.maximum_label_width());
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<usize> {
        self.selected
    }

    #[cfg(test)]
    pub(crate) fn view(&self) -> (i64, i64) {
        (self.view_start, self.view_span)
    }

    fn recompute_rows(&mut self) {
        self.rows = if self.show_all {
            self.waterfall.rows(&Folds::all_open(self.projection.activities.len()))
        } else {
            self.waterfall.rows(&self.folds)
        };
    }

    /// Selects an activity and tells the inspector, which follows the rows of its events.
    pub(crate) fn select(&mut self, activity: usize, cx: &mut Context<Self>) {
        if self.waterfall.span(activity).is_none() {
            return;
        }
        self.selected = Some(activity);
        let rows = self
            .projection
            .activities
            .get(activity)
            .map(|found| found.event_rows.clone())
            .unwrap_or_default();
        let result = self.result.clone();
        ActiveSelection::shared(cx).update(cx, |active, cx| {
            active.result = Some(result);
            active.table_index = 0;
            active.rows = rows;
            cx.notify();
        });
        cx.notify();
    }

    /// Expands and opens what hides an activity so it shows, selects it and scrolls to it.
    pub(crate) fn reveal(&mut self, activity: usize, cx: &mut Context<Self>) {
        if self.waterfall.span(activity).is_none() {
            return;
        }
        self.folds.reveal(&self.waterfall, &self.projection, activity);
        self.recompute_rows();
        self.select(activity, cx);
        if let Some(position) = self.rows.iter().position(|row| row.activity == activity) {
            self.scroll_handle.scroll_to_item(position, ScrollStrategy::Center);
        }
    }

    #[cfg(test)]
    pub(crate) fn has_context_menu(&self) -> bool {
        self.context_menu.is_some()
    }

    /// Selects the activity under a right click and offers to focus on it, when it has an id.
    fn deploy_context_menu(
        &mut self,
        activity: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(activity, cx);
        let Some(found) = self
            .projection
            .activities
            .get(activity)
            .filter(|found| found.has_activity_id)
        else {
            return;
        };
        let id = found.activity_id.clone();
        let view = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.entry("Focus on this activity", None, move |_, cx| {
                view.update(cx, |_, cx| cx.emit(WaterfallEvent::FocusRequested(id.clone())))
                    .log_err();
            })
        });
        window.focus(&menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.context_menu = None;
            cx.notify();
        });
        self.context_menu = Some((menu, position, subscription));
        cx.notify();
    }

    fn toggle_collapse(&mut self, activity: usize, cx: &mut Context<Self>) {
        if !self.folds.collapsed.remove(&activity) {
            self.folds.collapsed.insert(activity);
        }
        self.recompute_rows();
        cx.notify();
    }

    fn toggle_fold(&mut self, activity: usize, cx: &mut Context<Self>) {
        if !self.folds.opened.remove(&activity) {
            self.folds.opened.insert(activity);
        }
        self.recompute_rows();
        cx.notify();
    }

    fn toggle_show_all(&mut self, cx: &mut Context<Self>) {
        self.show_all = !self.show_all;
        self.recompute_rows();
        cx.notify();
    }

    /// Zooms about the middle of what is in view; a factor above 1 zooms in.
    fn zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        let extent = self.waterfall.extent.max(1);
        let smallest = (extent / SMALLEST_VIEW_SHARE).max(1);
        let middle = self.view_start + self.view_span / 2;
        let span = ((self.view_span as f64 / f64::from(factor)) as i64).clamp(smallest, extent);
        self.view_span = span;
        self.view_start = (middle - span / 2).clamp(0, extent - span);
        cx.notify();
    }

    fn fit(&mut self, cx: &mut Context<Self>) {
        self.view_start = 0;
        self.view_span = self.waterfall.extent.max(1);
        cx.notify();
    }

    /// Moves the view by a share of what it spans; positive goes later.
    fn pan(&mut self, share: f32, cx: &mut Context<Self>) {
        let extent = self.waterfall.extent.max(1);
        let moved = (self.view_span as f64 * f64::from(share)) as i64;
        self.view_start = (self.view_start + moved).clamp(0, extent - self.view_span);
        cx.notify();
    }

    fn on_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        let delta = event.delta.pixel_delta(ROW_HEIGHT);
        let (across, down) = (f32::from(delta.x), f32::from(delta.y));
        if event.modifiers.control || event.modifiers.platform {
            if down == 0.0 {
                return false;
            }
            let steps = (down / 20.0).clamp(-1.0, 1.0);
            self.zoom(ZOOM_STEP.powf(steps), cx);
            return true;
        }
        if event.modifiers.shift || across.abs() > down.abs() {
            let moved = if across != 0.0 { across } else { down };
            self.pan(-moved / 300.0, cx);
            return true;
        }
        false
    }

    fn fraction(&self, ticks: i64) -> f32 {
        (ticks - self.view_start) as f32 / self.view_span.max(1) as f32
    }

    fn tooltip_text(&self, activity: usize) -> Option<String> {
        let span = self.waterfall.span(activity)?;
        let found = self.projection.activities.get(activity)?;
        let mut lines = vec![span.marker.clone().unwrap_or_else(|| found.activity_id.clone())];
        if let Some(actor) = span.actor.and_then(|actor| self.waterfall.actors.get(actor)) {
            lines.push(format!("Actor {actor}"));
        }
        lines.push(format!(
            "Start {} · duration {}",
            offset_from_start(span.start),
            short_duration(span.duration())
        ));
        if let Some(untraced) = span.untraced.filter(|ticks| *ticks > 0) {
            lines.push(format!("{} not covered by traced work", short_duration(untraced)));
        }
        if span.critical > 0 {
            lines.push(format!("{} of its own time is on the critical path", short_duration(span.critical)));
        }
        lines.push(format!("{} events", span.event_count));
        if span.origin {
            lines.push("A failure began here".to_string());
        } else if span.on_failure_path {
            lines.push("On the path from a failure to the root".to_string());
        } else if span.handled_at.is_some() {
            lines.push("Logged an error and recovered".to_string());
        }
        lines.push("Times are from the logged events and include its descendants.".to_string());
        Some(lines.join("\n"))
    }

    fn render_row(&self, position: usize, palette: &[Hsla], cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = *self.rows.get(position)?;
        let span = self.waterfall.span(row.activity)?;
        let found = self.projection.activities.get(row.activity)?;
        let activity = row.activity;
        let colors = cx.theme().colors();
        let colour = span
            .actor
            .and_then(|actor| palette.get(actor).copied())
            .unwrap_or(colors.text_muted);
        let selected = self.selected == Some(activity);
        let folds_open = self.folds.opened.contains(&activity);
        let on_critical = span.critical > 0;
        let dimmed = self.highlight_critical && !on_critical;

        let name: SharedString = span
            .marker
            .as_deref()
            .map(short_marker)
            .unwrap_or_else(|| found.activity_id.clone())
            .into();

        let collapse = if row.collapsible && !self.show_all {
            div()
                .debug_selector(move || format!("waterfall-collapse-{position}"))
                .child(
                    IconButton::new(
                        ("waterfall-collapse", activity),
                        if row.collapsed { IconName::ChevronRight } else { IconName::ChevronDown },
                    )
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text(if row.collapsed {
                        "Show what is below this activity"
                    } else {
                        "Hide everything below this activity"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_collapse(activity, cx))),
                )
                .into_any_element()
        } else {
            div().w(px(20.)).into_any_element()
        };

        let label = h_flex()
            .w(self.shown_label_width())
            .flex_shrink_0()
            .h_full()
            .pl(INDENT * row.depth as f32)
            .pr_2()
            .gap_1()
            .items_center()
            .child(collapse)
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(colour))
            .child(div().flex_1().min_w_0().overflow_hidden().child(Label::new(name).single_line().size(LabelSize::Small)))
            .when(row.folded > 0 && !row.collapsed && !self.show_all, |label| {
                label.child(
                    div()
                        .debug_selector(move || format!("waterfall-folded-{position}"))
                        .child(
                            Button::new(("waterfall-folded", activity), format!("+{}", row.folded))
                                .size(ButtonSize::Compact)
                                .label_size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .toggle_state(folds_open)
                                .tooltip(Tooltip::text(if folds_open {
                                    format!("Fold the {} shorter activities again", row.folded)
                                } else {
                                    format!("{} shorter activities are folded into this one. Click to show them", row.folded)
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(activity, cx))),
                        ),
                )
            })
            .child(
                Label::new(short_duration(span.duration()))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            );

        let bar_colour = if dimmed { colour.opacity(0.3) } else { colour.opacity(0.9) };
        let mut bars = div()
            .debug_selector(move || format!("waterfall-bars-{position}"))
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden();
        bars = bars.child(
            div()
                .debug_selector(move || format!("waterfall-bar-{position}"))
                .absolute()
                .top((ROW_HEIGHT - BAR_HEIGHT) / 2.)
                .h(BAR_HEIGHT)
                .left(relative(self.fraction(span.start)))
                .w(relative(span.duration() as f32 / self.view_span.max(1) as f32))
                .min_w(MINIMUM_BAR_WIDTH)
                .rounded_sm()
                .bg(bar_colour)
                .when(self.highlight_critical && on_critical, |bar| {
                    bar.border_1().border_color(colors.text)
                }),
        );
        for (from, to) in self.waterfall.untraced_gaps(activity) {
            let width = (to - from) as f32 / self.view_span.max(1) as f32;
            bars = bars.child(
                div()
                    .absolute()
                    .top((ROW_HEIGHT - BAR_HEIGHT) / 2.)
                    .h(BAR_HEIGHT)
                    .left(relative(self.fraction(*from)))
                    .w(relative(width))
                    .bg(pattern_slash(colors.editor_background, HATCH_WIDTH, HATCH_INTERVAL)),
            );
        }
        for (at, colour) in [
            (span.error_at, cx.theme().status().error),
            (span.handled_at, cx.theme().status().warning),
        ] {
            if let Some(at) = at {
                bars = bars.child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .left(relative(self.fraction(at)))
                        .w(px(2.))
                        .bg(colour),
                );
            }
        }

        let tooltip = self.tooltip_text(activity).unwrap_or_default();
        Some(
            h_flex()
                .id(("waterfall-row", position))
                .debug_selector(move || format!("waterfall-row-{position}"))
                .w_full()
                .h(ROW_HEIGHT)
                .items_center()
                .cursor_pointer()
                .hover(|row| row.bg(colors.ghost_element_hover))
                .when(selected, |row| row.bg(colors.element_selected))
                .tooltip(Tooltip::text(tooltip))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.select(activity, cx);
                    if event.click_count() == 2 {
                        cx.emit(WaterfallEvent::OpenActivity(activity));
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.deploy_context_menu(activity, event.position, window, cx)
                    }),
                )
                .child(label)
                .child(bars)
                .into_any_element(),
        )
    }

    fn render_axis(&self, cx: &App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let step = nice_step(self.view_span);
        let mut ticks = Vec::new();
        let first = (self.view_start + step - 1).div_euclid(step).max(0) * step;
        let mut at = first;
        while at <= self.view_start + self.view_span && ticks.len() < 40 {
            ticks.push(at);
            at += step;
        }
        let mut axis = div().relative().flex_1().min_w_0().h_full().overflow_hidden();
        for tick in ticks {
            let label = if tick == 0 { "0".to_string() } else { short_duration(tick) };
            axis = axis.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .left(relative(self.fraction(tick)))
                    .pl_1()
                    .border_l_1()
                    .border_color(colors.border_variant)
                    .child(Label::new(label).size(LabelSize::XSmall).color(Color::Muted)),
            );
        }
        h_flex()
            .w_full()
            .h(px(22.))
            .border_b_1()
            .border_color(colors.border)
            .child(div().w(self.shown_label_width()).flex_shrink_0())
            .child(axis)
    }
}

/// One colour for each of `count` actors, all different, at one saturation and lightness. Past
/// the last hue they come round again, lighter and then darker, so that a trace with more actors
/// than hues still tells neighbours apart.
fn actor_palette(count: usize, saturation: f32, lightness: f32) -> Vec<Hsla> {
    (0..count)
        .map(|actor| {
            let shift = [0.0, 0.12, -0.12][(actor / ACTOR_HUES.len()) % 3];
            hsla(
                ACTOR_HUES[actor % ACTOR_HUES.len()] / 360.,
                saturation,
                (lightness + shift).clamp(0.25, 0.8),
                1.,
            )
        })
        .collect()
}

/// The actor colours for the current theme. The saturation and lightness are the average of the
/// theme's player colours, so they sit well on it, but the hues are not the players': a theme can
/// hold two of nearly the same colour (One Light has two purples), and the actors must not.
fn theme_actor_palette(count: usize, cx: &App) -> Vec<Hsla> {
    let mut saturation = 0.;
    let mut lightness = 0.;
    let mut players = 0.;
    // The first player is the local one, which a trace has no part for.
    for player in cx.theme().players().0.iter().skip(1) {
        saturation += player.cursor.s;
        lightness += player.cursor.l;
        players += 1.;
    }
    let (saturation, lightness) = if players > 0. {
        (saturation / players, lightness / players)
    } else {
        (0.5, 0.5)
    };
    actor_palette(count, saturation.clamp(0.35, 0.75), lightness.clamp(0.35, 0.7))
}

/// A round step in ticks for an axis over `span`, giving about `AXIS_TICKS` marks: 1, 2 or 5 times
/// a power of ten milliseconds.
fn nice_step(span: i64) -> i64 {
    const TICKS_PER_MILLISECOND: i64 = 10_000;
    let wanted = (span / AXIS_TICKS).max(TICKS_PER_MILLISECOND / 100);
    let mut unit = TICKS_PER_MILLISECOND / 100;
    loop {
        for multiple in [1, 2, 5] {
            if unit * multiple >= wanted {
                return unit * multiple;
            }
        }
        unit *= 10;
    }
}

impl Render for WaterfallView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let shown = self.rows.len();
        let total = self.waterfall.spans.iter().filter(|span| span.is_some()).count();
        let summary = format!(
            "{} · {shown} of {total} activities",
            short_duration(self.waterfall.extent)
        );
        let missing = self.waterfall.without_bounds;
        let highlight = self.highlight_critical;
        let show_all = self.show_all;

        v_flex()
            .id("waterfall")
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(Label::new("Timeline").weight(gpui::FontWeight::SEMIBOLD))
                    .child(Label::new(summary).size(LabelSize::Small).color(Color::Muted))
                    .when(missing > 0, |bar| {
                        bar.child(
                            Label::new(format!("{missing} activities without a readable timestamp are not drawn"))
                                .size(LabelSize::XSmall)
                                .color(Color::Warning),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div().debug_selector(|| "waterfall-critical".to_string()).child(
                            Button::new("waterfall-critical", "Critical path")
                                .size(ButtonSize::Compact)
                                .toggle_state(highlight)
                                .tooltip(Tooltip::text("Emphasise the activities that hold time on the critical path"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.highlight_critical = !this.highlight_critical;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "waterfall-show-all".to_string()).child(
                            Button::new("waterfall-show-all", "Show all")
                                .size(ButtonSize::Compact)
                                .toggle_state(show_all)
                                .tooltip(Tooltip::text("Open every fold, so no activity is hidden"))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_show_all(cx))),
                        ),
                    )
                    .child(
                        Button::new("waterfall-zoom-out", "−")
                            .size(ButtonSize::Compact)
                            .tooltip(Tooltip::text("Zoom out. Ctrl or Cmd with the wheel also zooms; Shift with the wheel moves along the time axis"))
                            .on_click(cx.listener(|this, _, _, cx| this.zoom(1.0 / ZOOM_STEP, cx))),
                    )
                    .child(
                        div().debug_selector(|| "waterfall-zoom-in".to_string()).child(
                            Button::new("waterfall-zoom-in", "+")
                                .size(ButtonSize::Compact)
                                .tooltip(Tooltip::text("Zoom in"))
                                .on_click(cx.listener(|this, _, _, cx| this.zoom(ZOOM_STEP, cx))),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "waterfall-fit".to_string()).child(
                            Button::new("waterfall-fit", "Fit")
                                .size(ButtonSize::Compact)
                                .tooltip(Tooltip::text("Show the whole trace"))
                                .on_click(cx.listener(|this, _, _, cx| this.fit(cx))),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .on_drag_move(
                        cx.listener(|this, event: &DragMoveEvent<LabelSplitterDrag>, _, cx| {
                            this.set_label_width(event.event.position.x - event.bounds.left(), cx)
                        }),
                    )
                    .child(self.render_axis(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                                if this.on_wheel(event, cx) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                uniform_list(
                                    "waterfall-rows",
                                    self.rows.len(),
                                    cx.processor(|this, range: Range<usize>, _window, cx| {
                                        let palette = theme_actor_palette(this.waterfall.actors.len(), cx);
                                        range
                                            .filter_map(|position| this.render_row(position, &palette, cx))
                                            .collect::<Vec<_>>()
                                    }),
                                )
                                .track_scroll(&self.scroll_handle)
                                .size_full(),
                            ),
                    )
                    // Over the rows, so it must not let a click through to the one beneath it,
                    // which a double-click would open; the wheel still reaches the rows.
                    .child(
                        h_flex()
                            .id("waterfall-splitter")
                            .debug_selector(|| "waterfall-splitter".to_string())
                            .absolute()
                            .top_0()
                            .h_full()
                            .left(self.shown_label_width() - SPLITTER_WIDTH / 2.)
                            .w(SPLITTER_WIDTH)
                            .justify_center()
                            .cursor_col_resize()
                            .block_mouse_except_scroll()
                            .hover(|style| style.bg(colors.border_focused.opacity(0.4)))
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                                if event.click_count() == 2 {
                                    this.set_label_width(DEFAULT_LABEL_WIDTH, cx);
                                }
                            }))
                            .on_drag(LabelSplitterDrag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                            .child(div().w(px(1.)).h_full().bg(colors.border_variant)),
                    )
                    .child({
                        let bounds = self.bounds.clone();
                        canvas(move |measured, _, _| bounds.set(measured), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    }),
            )
            .children(self.context_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    anchored()
                        .position(*position)
                        .anchor(Anchor::TopLeft)
                        .child(menu.clone()),
                )
                .with_priority(1)
            }))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{
        Modifiers, MouseUpEvent, TestAppContext, VisualTestContext, size,
    };
    use kusto_results::activity::build_projection_with;
    use kusto_results::trace_schema::TraceColumns;
    use kusto_results::waterfall::WaterfallOptions;
    use kusto_results::{Cell, Column, Table};

    use super::*;

    /// Run lasts 10 s and Big runs inside it from 1 s to 9 s.
    fn open(cx: &mut TestAppContext) -> (Entity<WaterfallView>, &mut VisualTestContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
        let events: &[(&str, &str, &str, i64)] = &[
            ("r", "", "Run", 0),
            ("r", "", "Run", 10),
            ("big", "r", "Big", 1),
            ("big", "r", "Big", 9),
        ];
        let text = |value: &str| {
            if value.is_empty() {
                Cell::Null
            } else {
                Cell::Text(value.into())
            }
        };
        let result = Arc::new(ResultSet {
            tables: vec![Table {
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
                    .iter()
                    .map(|(activity, parent, marker, seconds)| {
                        vec![
                            text(activity),
                            text(parent),
                            text("A"),
                            text(marker),
                            Cell::Text(format!("2026-01-01T00:00:{seconds:02}.0000000Z")),
                            Cell::Int(4),
                            text("message"),
                        ]
                    })
                    .collect(),
            }],
            ..Default::default()
        });
        let table = &result.tables[0];
        let columns = TraceColumns::detect(table);
        let projection = Arc::new(build_projection_with(table, &columns).expect("a projection"));
        let waterfall = Arc::new(
            Waterfall::build(table, &projection, &columns, &WaterfallOptions::default())
                .expect("a waterfall"),
        );
        let (view, cx) = cx.add_window_view(|_, _| WaterfallView::new(result.clone(), projection, waterfall));
        cx.simulate_resize(size(px(1000.), px(600.)));
        draw(cx);
        (view, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn splitter(cx: &mut VisualTestContext) -> Point<Pixels> {
        draw(cx);
        cx.debug_bounds("waterfall-splitter")
            .map(|bounds| bounds.center())
            .expect("the splitter shows")
    }

    fn drag_splitter(cx: &mut VisualTestContext, by: Pixels) {
        let handle = splitter(cx);
        cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::default());
        for step in 1..=6 {
            cx.simulate_mouse_move(
                Point { x: handle.x + by * (step as f32 / 6.), y: handle.y },
                MouseButton::Left,
                Modifiers::default(),
            );
        }
        cx.simulate_mouse_up(
            Point { x: handle.x + by, y: handle.y },
            MouseButton::Left,
            Modifiers::default(),
        );
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

    /// Where the bars of the first row start, from the left of the row.
    fn bars_start(cx: &mut VisualTestContext) -> f32 {
        draw(cx);
        let row = cx.debug_bounds("waterfall-row-0").expect("the first row shows");
        let bars = cx.debug_bounds("waterfall-bars-0").expect("its bars show");
        f32::from(bars.origin.x - row.origin.x)
    }

    #[gpui::test]
    async fn the_splitter_resizes_the_labels_within_its_limits(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let width = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.label_width());
        assert_eq!(width(cx), px(360.));
        assert!((bars_start(cx) - 360.0).abs() < 1.0, "the bars start where the labels end");

        drag_splitter(cx, px(100.));
        assert!(width(cx) > px(440.) && width(cx) < px(480.), "the drag widened it: {:?}", width(cx));
        assert!(
            (bars_start(cx) - f32::from(width(cx))).abs() < 1.0,
            "the bars follow the labels: {} against {:?}",
            bars_start(cx),
            width(cx)
        );

        drag_splitter(cx, px(-2000.));
        assert_eq!(width(cx), MINIMUM_LABEL_WIDTH, "the labels keep their minimum");

        drag_splitter(cx, px(5000.));
        assert_eq!(width(cx), px(1000.) - MINIMUM_BARS_WIDTH, "the bars keep their minimum");

        let handle = splitter(cx);
        double_click(cx, handle);
        assert_eq!(width(cx), DEFAULT_LABEL_WIDTH, "double-click resets it");
    }

    #[gpui::test]
    async fn a_double_click_on_the_splitter_does_not_open_the_row_under_it(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        let opened = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|_, cx| {
            let opened = opened.clone();
            cx.subscribe(&view, move |_, event: &WaterfallEvent, _| {
                if let WaterfallEvent::OpenActivity(activity) = event {
                    opened.borrow_mut().push(*activity);
                }
            })
        });
        let handle = splitter(cx);
        let row = cx.debug_bounds("waterfall-row-0").expect("the first row shows");
        double_click(cx, Point { x: handle.x, y: row.center().y });
        assert!(opened.borrow().is_empty(), "nothing was opened: {:?}", opened.borrow());

        double_click(cx, Point { x: row.right() - px(20.), y: row.center().y });
        assert_eq!(opened.borrow().len(), 1, "a double-click on the row itself still opens it");
    }

    #[test]
    fn no_two_actor_hues_are_closer_than_24_degrees() {
        let mut hues = ACTOR_HUES.to_vec();
        hues.sort_by(f32::total_cmp);
        let around = hues.windows(2).map(|pair| pair[1] - pair[0]).chain([hues[0] + 360. - hues[hues.len() - 1]]);
        let closest = around.fold(f32::MAX, f32::min);
        assert!(closest >= 24., "the closest hues are {closest} degrees apart");
    }

    #[test]
    fn every_actor_gets_a_colour_of_its_own_until_the_hues_run_out_and_then_a_lighter_or_darker_one() {
        for (saturation, lightness) in [(0.46, 0.46), (0.47, 0.64)] {
            let palette = actor_palette(ACTOR_HUES.len() * 3, saturation, lightness);
            assert_eq!(palette.len(), 30);
            for (first, a) in palette.iter().enumerate() {
                for b in &palette[first + 1..] {
                    assert!((a.h, a.s, a.l) != (b.h, b.s, b.l), "{a:?} appears twice");
                }
            }
            let (first_round, second_round) = (palette[0], palette[ACTOR_HUES.len()]);
            assert_eq!(first_round.h, second_round.h, "the same hue comes round again");
            assert!(second_round.l > first_round.l, "lighter the second time");
            assert!(palette[ACTOR_HUES.len() * 2].l < first_round.l, "darker the third time");
        }
        assert!(actor_palette(0, 0.5, 0.5).is_empty());
    }

    #[gpui::test]
    async fn the_actors_of_a_trace_are_drawn_in_different_colours(cx: &mut TestAppContext) {
        let (view, cx) = open(cx);
        cx.update(|_, cx| {
            let palette = theme_actor_palette(view.read(cx).waterfall.actors.len().max(2), cx);
            assert_ne!(palette[0], palette[1]);
            assert!(palette.iter().all(|colour| colour.s >= 0.35 && colour.s <= 0.75));
        });
    }

    #[test]
    fn the_axis_steps_are_round_and_give_about_eight_marks() {
        let ms = 10_000i64;
        assert_eq!(nice_step(4_720 * ms), 1_000 * ms, "4.72 s over 8 is 590 ms, rounded up to 1 s");
        assert_eq!(nice_step(1_000 * ms), 200 * ms);
        assert_eq!(nice_step(70 * ms), 10 * ms);
        assert_eq!(nice_step(60_000 * ms), 10_000 * ms);
        assert!(nice_step(1) >= 100, "a one tick view still has a step");
    }
}
