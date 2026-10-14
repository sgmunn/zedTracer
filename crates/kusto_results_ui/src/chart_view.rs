//! The tab of a chart: the table drawn as lines or bars on a canvas, with a tooltip under the
//! pointer and a legend whose entries switch series on and off (CVW, CIN).
//!
//! The scene is laid out at the size the canvas has, in `prepaint`, because that is the first
//! place where both the size and the width of text are known. It is kept until the size, the
//! data or the options change, so moving the pointer only paints.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, BorderStyle, Bounds, ContentMask, Context, Font, FontWeight, Hsla,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels, Point, Render,
    SharedString, Task, TextAlign, TextRun, Window, canvas, div, fill, outline, point, px, size,
};
use gpui_util::ResultExt as _;
use kusto_results::chart_data::ChartData;
use kusto_results::chart_scene::{
    Anchor, ChartScene, Hover, Item, Rect, Role, TextMeasure, VerticalAnchor, ViewState,
};
use kusto_results::{Chart, ChartOptions, ResultSet};
use ui::prelude::*;

use crate::actor_colours::theme_actor_palette;

const TOOLTIP_OFFSET: Pixels = px(12.);
const TOOLTIP_MAXIMUM_NAME_WIDTH: Pixels = px(240.);

enum DataState {
    Building { _task: Task<()> },
    Ready(Arc<ChartData>),
    Problem(SharedString),
}

/// What the canvas needs, shared with its closures, which outlive a call to `render`.
struct Painted {
    data: Option<Arc<ChartData>>,
    options: ChartOptions,
    view: ViewState,
    /// Counts the changes that need the scene laid out again.
    generation: u64,
    series_colors: usize,
    container: Bounds<Pixels>,
    /// Where the chart is drawn: the container, or the part of it the aspect ratio leaves.
    area: Bounds<Pixels>,
    built_for: Option<(u32, u32, u64)>,
    scene: Option<ChartScene>,
    hover: Option<Hover>,
}

impl Painted {
    fn fit(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let Some(ratio) = self
            .options
            .aspect_ratio
            .and_then(kusto_results::chart::AspectRatio::ratio)
        else {
            return bounds;
        };
        let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if width <= 0.0 || height <= 0.0 {
            return bounds;
        }
        let (fitted_width, fitted_height) = if width / height > ratio {
            (height * ratio, height)
        } else {
            (width, width / ratio)
        };
        Bounds::new(
            point(
                bounds.origin.x + px((width - fitted_width) / 2.0),
                bounds.origin.y + px((height - fitted_height) / 2.0),
            ),
            size(px(fitted_width), px(fitted_height)),
        )
    }

    /// Lays the scene out when its size, data or options are not the ones it was laid out for.
    fn prepare(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        self.container = bounds;
        self.area = self.fit(bounds);
        let Some(data) = self.data.clone() else {
            self.scene = None;
            return;
        };
        let (width, height) = (
            f32::from(self.area.size.width),
            f32::from(self.area.size.height),
        );
        let key = (width.round() as u32, height.round() as u32, self.generation);
        if self.built_for == Some(key) {
            return;
        }
        let font = theme::theme_settings(cx).ui_font(cx).clone();
        let measure = WindowText {
            window: &*window,
            font,
        };
        self.scene = Some(ChartScene::build(
            &data,
            &self.options,
            &self.view,
            width,
            height,
            &measure,
        ));
        self.built_for = Some(key);
        self.hover = None;
    }

    fn paint(&self, window: &mut Window, cx: &mut App) {
        let Some(scene) = &self.scene else {
            return;
        };
        let colours = Colours::of(self.series_colors, cx);
        let font = theme::theme_settings(cx).ui_font(cx).clone();
        let origin = self.area.origin;
        let plot = to_bounds(scene.plot, origin);

        for item in &scene.grid {
            paint_item(item, origin, &colours, &font, window, cx);
        }
        window.with_content_mask(Some(ContentMask { bounds: plot }), |window| {
            for item in &scene.plot_items {
                paint_item(item, origin, &colours, &font, window, cx);
            }
            if let Some(hover) = &self.hover {
                if let Some(x) = hover.crosshair_x {
                    let crosshair = Item::Segment {
                        from: (x.floor() + 0.5, scene.plot.top),
                        to: (x.floor() + 0.5, scene.plot.bottom()),
                        role: Role::Axis,
                        width: 1.0,
                    };
                    paint_item(&crosshair, origin, &colours, &font, window, cx);
                }
                for (x, y, color_index) in &hover.marks {
                    let mark = Item::Circle {
                        center: (*x, *y),
                        radius: 4.0,
                        role: Role::Series(*color_index),
                    };
                    paint_item(&mark, origin, &colours, &font, window, cx);
                }
                if let Some(bar) = hover.bar {
                    window.paint_quad(outline(
                        to_bounds(bar, origin),
                        colours.text,
                        BorderStyle::Solid,
                    ));
                }
            }
        });
        for item in &scene.chrome {
            paint_item(item, origin, &colours, &font, window, cx);
        }
    }

    fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        let local = position - self.area.origin;
        (f32::from(local.x), f32::from(local.y))
    }
}

struct WindowText<'a> {
    window: &'a Window,
    font: Font,
}

impl TextMeasure for WindowText<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        shape(self.window, &self.font, text, size, Hsla::default(), false)
            .width
            .into()
    }
}

fn shape(
    window: &Window,
    font: &Font,
    text: &str,
    size: f32,
    color: Hsla,
    bold: bool,
) -> gpui::ShapedLine {
    let mut font = font.clone();
    if bold {
        font.weight = FontWeight::BOLD;
    }
    let run = TextRun {
        len: text.len(),
        font,
        color,
        ..Default::default()
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), px(size), &[run], None)
}

struct Colours {
    text: Hsla,
    muted: Hsla,
    grid: Hsla,
    axis: Hsla,
    series: Vec<Hsla>,
}

impl Colours {
    fn of(series_count: usize, cx: &App) -> Self {
        let colors = cx.theme().colors();
        Self {
            text: colors.text,
            muted: colors.text_muted,
            grid: colors.border_variant,
            axis: colors.border,
            series: theme_actor_palette(series_count.max(1), cx),
        }
    }

    fn of_role(&self, role: Role) -> Hsla {
        match role {
            Role::Text => self.text,
            Role::MutedText => self.muted,
            Role::Grid => self.grid,
            Role::Axis => self.axis,
            Role::Series(index) => self
                .series
                .get(index % self.series.len().max(1))
                .copied()
                .unwrap_or(self.text),
        }
    }
}

fn to_bounds(rect: Rect, origin: Point<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(rect.left), origin.y + px(rect.top)),
        size(px(rect.width), px(rect.height)),
    )
}

fn to_point(position: (f32, f32), origin: Point<Pixels>) -> Point<Pixels> {
    point(origin.x + px(position.0), origin.y + px(position.1))
}

fn paint_item(
    item: &Item,
    origin: Point<Pixels>,
    colours: &Colours,
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    match item {
        Item::Segment {
            from,
            to,
            role,
            width,
        } => {
            let color = colours.of_role(*role);
            if from.0 == to.0 || from.1 == to.1 {
                // Lines along the axes are thin quads, which stay sharp.
                let rect = Rect {
                    left: from.0.min(to.0) - if from.0 == to.0 { width / 2.0 } else { 0.0 },
                    top: from.1.min(to.1) - if from.1 == to.1 { width / 2.0 } else { 0.0 },
                    width: if from.0 == to.0 {
                        *width
                    } else {
                        (to.0 - from.0).abs()
                    },
                    height: if from.1 == to.1 {
                        *width
                    } else {
                        (to.1 - from.1).abs()
                    },
                };
                window.paint_quad(fill(to_bounds(rect, origin), color));
            } else {
                let mut builder = PathBuilder::stroke(px(*width));
                builder.move_to(to_point(*from, origin));
                builder.line_to(to_point(*to, origin));
                if let Some(path) = builder.build().log_err() {
                    window.paint_path(path, color);
                }
            }
        }
        Item::Polyline {
            points,
            role,
            width,
        } => {
            let mut points = points.iter();
            let Some(first) = points.next() else {
                return;
            };
            let mut builder = PathBuilder::stroke(px(*width));
            builder.move_to(to_point(*first, origin));
            for position in points {
                builder.line_to(to_point(*position, origin));
            }
            if let Some(path) = builder.build().log_err() {
                window.paint_path(path, colours.of_role(*role));
            }
        }
        Item::Rect {
            rect,
            role,
            opacity,
        } => {
            window.paint_quad(fill(
                to_bounds(*rect, origin),
                colours.of_role(*role).opacity(*opacity),
            ));
        }
        Item::Circle {
            center,
            radius,
            role,
        } => {
            let rect = Rect {
                left: center.0 - radius,
                top: center.1 - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            };
            window.paint_quad(
                fill(to_bounds(rect, origin), colours.of_role(*role)).corner_radii(px(*radius)),
            );
        }
        Item::Text {
            text,
            at,
            size,
            horizontal,
            vertical,
            role,
            bold,
        } => {
            if text.is_empty() {
                return;
            }
            let line = shape(window, font, text, *size, colours.of_role(*role), *bold);
            let width = f32::from(line.width);
            let line_height = *size * 1.3;
            let left = match horizontal {
                Anchor::Start => at.0,
                Anchor::Middle => at.0 - width / 2.0,
                Anchor::End => at.0 - width,
            };
            let top = match vertical {
                VerticalAnchor::Top => at.1,
                VerticalAnchor::Middle => at.1 - line_height / 2.0,
                VerticalAnchor::Bottom => at.1 - line_height,
            };
            line.paint(
                to_point((left, top), origin),
                px(line_height),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .log_err();
        }
    }
}

/// The data a chart draws, or why there is none.
fn build_data(result: &ResultSet, chart: &Chart) -> Result<ChartData, SharedString> {
    let table = result.chart_table(chart).ok_or_else(|| {
        SharedString::from(format!(
            "The table {} is not in this result.",
            chart.table_name.as_deref().unwrap_or_default()
        ))
    })?;
    ChartData::build(table, &chart.options)
        .map_err(|problem| SharedString::from(problem.to_string()))
}

pub(crate) struct ChartView {
    result: Arc<ResultSet>,
    chart: Chart,
    data: DataState,
    painted: Rc<RefCell<Painted>>,
}

impl ChartView {
    pub(crate) fn new(
        result: Arc<ResultSet>,
        chart: Chart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let painted = Rc::new(RefCell::new(Painted {
            data: None,
            options: chart.options.clone(),
            view: ViewState::default(),
            generation: 0,
            series_colors: 0,
            container: Bounds::default(),
            area: Bounds::default(),
            built_for: None,
            scene: None,
            hover: None,
        }));
        let mut view = Self {
            result,
            chart,
            data: DataState::Problem(SharedString::default()),
            painted,
        };
        view.build_data(window, cx);
        view
    }

    #[cfg(test)]
    pub(crate) fn chart(&self) -> &Chart {
        &self.chart
    }

    /// Reads the table away from the window, so a large result does not stall typing.
    fn build_data(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.result.clone();
        let chart = self.chart.clone();
        self.data = DataState::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let built = cx
                    .background_spawn(async move { build_data(&result, &chart) })
                    .await;
                this.update(cx, |this, cx| {
                    this.data = match built {
                        Ok(data) => {
                            let data = Arc::new(data);
                            let mut painted = this.painted.borrow_mut();
                            painted.series_colors = data
                                .series
                                .iter()
                                .map(|series| series.color_index + 1)
                                .max()
                                .unwrap_or(0);
                            painted.data = Some(data.clone());
                            painted.generation += 1;
                            DataState::Ready(data)
                        }
                        Err(problem) => {
                            this.painted.borrow_mut().data = None;
                            DataState::Problem(problem)
                        }
                    };
                    cx.notify();
                })
                .log_err();
            }),
        };
    }

    fn pointer_moved(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let mut painted = self.painted.borrow_mut();
        let (x, y) = painted.local(position);
        let hover = painted.scene.as_ref().and_then(|scene| scene.hover(x, y));
        if hover != painted.hover {
            painted.hover = hover;
            cx.notify();
        }
    }

    fn pointer_left(&mut self, cx: &mut Context<Self>) {
        if self.painted.borrow_mut().hover.take().is_some() {
            cx.notify();
        }
    }

    fn clicked(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let mut painted = self.painted.borrow_mut();
        let (x, y) = painted.local(position);
        let entry = painted
            .scene
            .as_ref()
            .and_then(|scene| scene.legend_entry_at(x, y));
        if let Some(color_index) = entry {
            painted.view.toggle(color_index);
            painted.generation += 1;
            painted.hover = None;
            cx.notify();
        }
    }

    fn render_tooltip(&self, cx: &App) -> Option<AnyElement> {
        let painted = self.painted.borrow();
        let hover = painted.hover.as_ref()?;
        let colors = cx.theme().colors();
        let palette = theme_actor_palette(painted.series_colors.max(1), cx);
        let offset = painted.area.origin - painted.container.origin;
        let x = offset.x + px(hover.anchor.0);
        let y = offset.y + px(hover.anchor.1);
        let to_the_left = px(hover.anchor.0) > painted.area.size.width / 2.0;
        let rows: Vec<_> = hover
            .entries
            .iter()
            .map(|entry| {
                let color = palette
                    .get(entry.color_index % palette.len().max(1))
                    .copied()
                    .unwrap_or(colors.text);
                h_flex()
                    .gap_2()
                    .child(div().size(px(8.)).rounded_full().bg(color).flex_none())
                    .child(
                        div()
                            .max_w(TOOLTIP_MAXIMUM_NAME_WIDTH)
                            .overflow_hidden()
                            .child(
                                Label::new(entry.name.clone())
                                    .size(LabelSize::Small)
                                    .truncate(),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Label::new(entry.value.clone())
                            .size(LabelSize::Small)
                            .color(Color::Default),
                    )
            })
            .collect();
        Some(
            v_flex()
                .absolute()
                .top(y + TOOLTIP_OFFSET)
                .when(to_the_left, |tooltip| {
                    tooltip.right(painted.container.size.width - x + TOOLTIP_OFFSET)
                })
                .when(!to_the_left, |tooltip| tooltip.left(x + TOOLTIP_OFFSET))
                .gap_0p5()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(colors.border)
                .bg(colors.elevated_surface_background)
                .shadow_md()
                .child(
                    Label::new(hover.title.clone())
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    #[cfg(test)]
    pub(crate) fn data(&self) -> Option<Arc<ChartData>> {
        self.painted.borrow().data.clone()
    }

    #[cfg(test)]
    pub(crate) fn scene_texts(&self) -> Vec<String> {
        self.painted
            .borrow()
            .scene
            .as_ref()
            .map(|scene| {
                scene
                    .chrome
                    .iter()
                    .filter_map(|item| match item {
                        Item::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn scene(&self) -> Option<ChartScene> {
        self.painted.borrow().scene.clone()
    }

    #[cfg(test)]
    pub(crate) fn hover(&self) -> Option<Hover> {
        self.painted.borrow().hover.clone()
    }

    #[cfg(test)]
    pub(crate) fn area(&self) -> Bounds<Pixels> {
        self.painted.borrow().area
    }

    #[cfg(test)]
    pub(crate) fn problem(&self) -> Option<SharedString> {
        match &self.data {
            DataState::Problem(problem) => Some(problem.clone()),
            _ => None,
        }
    }
}

impl Render for ChartView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notes: Vec<SharedString> = match &self.data {
            DataState::Ready(data) => data
                .notes
                .iter()
                .map(|note| SharedString::from(note.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let body = match &self.data {
            DataState::Building { .. } => div()
                .p_4()
                .child(Label::new("Drawing the chart…"))
                .into_any_element(),
            DataState::Problem(problem) => div()
                .p_4()
                .child(Label::new(problem.clone()).color(Color::Muted))
                .into_any_element(),
            DataState::Ready(_) => {
                let prepared = self.painted.clone();
                let painted = self.painted.clone();
                div()
                    .id("chart")
                    .relative()
                    .size_full()
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.pointer_moved(event.position, cx)
                    }))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if !*hovered {
                            this.pointer_left(cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.clicked(event.position, cx)
                        }),
                    )
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                prepared.borrow_mut().prepare(bounds, window, cx)
                            },
                            move |_, _, window, cx| painted.borrow().paint(window, cx),
                        )
                        .size_full(),
                    )
                    .children(self.render_tooltip(cx))
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(body))
            .when(!notes.is_empty(), |view| {
                view.child(
                    div().px_3().py_1().children(
                        notes.into_iter().map(|note| {
                            Label::new(note).size(LabelSize::XSmall).color(Color::Muted)
                        }),
                    ),
                )
            })
    }
}
