//! The Timeline tab: a trace as rows of bars on a shared time axis, where the time went (WFL).
//!
//! Bars are placed with fractions of the row's width, so nothing here needs to measure the
//! window: zooming changes which part of the trace the fractions span.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Context, EventEmitter, Hsla, IntoElement, Render, ScrollStrategy,
    ScrollWheelEvent, SharedString, UniformListScrollHandle, Window, div, pattern_slash, px,
    relative, uniform_list,
};
use kusto_results::ResultSet;
use kusto_results::activity::ActivityProjection;
use kusto_results::trace_text::{offset_from_start, short_duration, short_marker};
use kusto_results::waterfall::{Row, Span, Waterfall};
use ui::{Button, ButtonSize, IconButton, IconName, IconSize, Tooltip, prelude::*};

use crate::row_details_panel::ActiveSelection;

const ROW_HEIGHT: Pixels = px(24.);
const LABEL_WIDTH: Pixels = px(360.);
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

pub(crate) enum WaterfallEvent {
    /// A row was double-clicked: show this activity's events.
    OpenActivity(usize),
}

impl EventEmitter<WaterfallEvent> for WaterfallView {}

pub(crate) struct WaterfallView {
    result: Arc<ResultSet>,
    projection: Arc<ActivityProjection>,
    waterfall: Arc<Waterfall>,
    /// Rows whose folded children are open.
    opened: HashSet<usize>,
    /// Open every fold, so nothing is hidden.
    show_all: bool,
    rows: Vec<Row>,
    selected: Option<usize>,
    /// The part of the trace in view, in ticks from its start.
    view_start: i64,
    view_span: i64,
    highlight_critical: bool,
    scroll_handle: UniformListScrollHandle,
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
            opened: HashSet::new(),
            show_all: false,
            rows: Vec::new(),
            selected: None,
            highlight_critical: false,
            scroll_handle: UniformListScrollHandle::new(),
        };
        view.recompute_rows();
        view
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
            let everything: HashSet<usize> = (0..self.projection.activities.len()).collect();
            self.waterfall.rows(&everything)
        } else {
            self.waterfall.rows(&self.opened)
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

    /// Opens the folds above an activity so it shows, selects it and scrolls to it.
    pub(crate) fn reveal(&mut self, activity: usize, cx: &mut Context<Self>) {
        if self.waterfall.span(activity).is_none() {
            return;
        }
        let mut current = self.projection.activities.get(activity).and_then(|found| found.parent);
        while let Some(parent) = current {
            self.opened.insert(parent);
            current = self.projection.activities.get(parent).and_then(|found| found.parent);
        }
        self.recompute_rows();
        self.select(activity, cx);
        if let Some(position) = self.rows.iter().position(|row| row.activity == activity) {
            self.scroll_handle.scroll_to_item(position, ScrollStrategy::Center);
        }
    }

    fn toggle_fold(&mut self, activity: usize, cx: &mut Context<Self>) {
        if !self.opened.remove(&activity) {
            self.opened.insert(activity);
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

    fn actor_colour(&self, span: &Span, cx: &App) -> Hsla {
        match span.actor {
            Some(actor) => cx.theme().players().color_for_participant(actor as u32).cursor,
            None => cx.theme().colors().text_muted,
        }
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

    fn render_row(&self, position: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = *self.rows.get(position)?;
        let span = self.waterfall.span(row.activity)?;
        let found = self.projection.activities.get(row.activity)?;
        let activity = row.activity;
        let colors = cx.theme().colors();
        let colour = self.actor_colour(span, cx);
        let selected = self.selected == Some(activity);
        let folds_open = self.opened.contains(&activity) || self.show_all;
        let can_fold = row.folded > 0 || (folds_open && !self.waterfall.folded_children(activity).is_empty());
        let on_critical = span.critical > 0;
        let dimmed = self.highlight_critical && !on_critical;

        let name: SharedString = span
            .marker
            .as_deref()
            .map(short_marker)
            .unwrap_or_else(|| found.activity_id.clone())
            .into();

        let fold = if can_fold && !self.show_all {
            IconButton::new(("waterfall-fold", activity), if folds_open { IconName::ChevronDown } else { IconName::ChevronRight })
                .icon_size(IconSize::XSmall)
                .tooltip(Tooltip::text(if folds_open {
                    "Fold the shorter activities again".to_string()
                } else {
                    format!("{} shorter activities are folded into this one. Click to show them", row.folded)
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(activity, cx)))
                .into_any_element()
        } else {
            div().w(px(20.)).into_any_element()
        };

        let label = h_flex()
            .w(LABEL_WIDTH)
            .flex_shrink_0()
            .h_full()
            .pl(INDENT * row.depth as f32)
            .pr_2()
            .gap_1()
            .items_center()
            .child(fold)
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(colour))
            .child(div().flex_1().min_w_0().overflow_hidden().child(Label::new(name).single_line().size(LabelSize::Small)))
            .when(row.folded > 0 && !folds_open, |label| {
                label.child(
                    Label::new(format!("+{}", row.folded))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
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
            .child(div().w(LABEL_WIDTH).flex_shrink_0())
            .child(axis)
    }
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
                                range
                                    .filter_map(|position| this.render_row(position, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.scroll_handle)
                        .size_full(),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
