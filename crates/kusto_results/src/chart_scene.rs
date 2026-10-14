//! Lays a chart out as drawing primitives: gridlines, lines, bars, labels and a legend at
//! pixel positions, and the lookup that says what the pointer is over.
//!
//! Nothing here draws. A view paints the items with its own toolkit, and the text measure it
//! supplies tells the layout how wide a label is. Text is never rotated, so a crowded axis
//! shows fewer labels instead of tilting them.
//!
//! The options that name an axis (`xAxis`, `yTitle`, ...) mean the axis on the screen, as they
//! do in the VS Code extension, so for a horizontal bar chart `x` is the axis of the values.

use std::collections::HashSet;

use serde_json::Value;

use crate::chart::{AxisScale, ChartOptions, LegendPosition, TextSize};
use crate::chart_data::{ChartData, Series, XKind, parse_datetime_text, parse_timespan_text};
use crate::chart_scale::{self, Tick};

/// How wide text is when drawn at a size, in pixels.
pub trait TextMeasure {
    fn width(&self, text: &str, size: f32) -> f32;
}

/// A guess at text width, for tests and for writing SVG where no font is at hand.
pub struct EstimatedText;

impl TextMeasure for EstimatedText {
    fn width(&self, text: &str, size: f32) -> f32 {
        text.chars().count() as f32 * size * 0.55
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.left + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x <= self.right() && y >= self.top && y <= self.bottom()
    }

    fn inflated(&self, amount: f32) -> Rect {
        Rect {
            left: self.left - amount,
            top: self.top - amount,
            width: self.width + 2.0 * amount,
            height: self.height + 2.0 * amount,
        }
    }
}

/// Which colour of the theme an item takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Text,
    MutedText,
    Grid,
    Axis,
    /// The colour of the series with this `color_index`.
    Series(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalAnchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Segment {
        from: (f32, f32),
        to: (f32, f32),
        role: Role,
        width: f32,
    },
    Polyline {
        points: Vec<(f32, f32)>,
        role: Role,
        width: f32,
    },
    Rect {
        rect: Rect,
        role: Role,
        opacity: f32,
    },
    Circle {
        center: (f32, f32),
        radius: f32,
        role: Role,
    },
    Text {
        text: String,
        at: (f32, f32),
        size: f32,
        horizontal: Anchor,
        vertical: VerticalAnchor,
        role: Role,
        bold: bool,
    },
}

/// A legend entry and where it sits, so that a click on it can be told from a click elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendEntry {
    /// `None` for the entry that says how many more there are.
    pub color_index: Option<usize>,
    pub name: String,
    pub hidden: bool,
    pub rect: Rect,
}

/// What the user has done to a chart without changing it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewState {
    /// The series switched off, by `color_index`, which stays with a series when the data is
    /// built again.
    pub hidden: HashSet<usize>,
}

impl ViewState {
    pub fn toggle(&mut self, color_index: usize) {
        if !self.hidden.remove(&color_index) {
            self.hidden.insert(color_index);
        }
    }
}

/// What lies under the pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct Hover {
    /// The X of a line chart, or the category of a bar.
    pub title: String,
    pub entries: Vec<HoverEntry>,
    /// Where to draw a vertical line through the plot, for a line chart.
    pub crosshair_x: Option<f32>,
    /// The points to mark, for a line chart, with the colour index of their series.
    pub marks: Vec<(f32, f32, usize)>,
    /// The bar to outline, for a bar chart.
    pub bar: Option<Rect>,
    /// Where a tooltip points.
    pub anchor: (f32, f32),
    /// The row of the table that the first entry came from.
    pub row: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HoverEntry {
    pub color_index: usize,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
struct HoverPoint {
    pixel: (f32, f32),
    x: f64,
    y: f64,
    row: usize,
}

#[derive(Debug, Clone)]
struct HoverSeries {
    color_index: usize,
    name: String,
    /// In order of x.
    points: Vec<HoverPoint>,
}

#[derive(Debug, Clone)]
struct BarHit {
    rect: Rect,
    color_index: usize,
    series: String,
    category: String,
    value: f64,
    row: usize,
}

#[derive(Debug, Clone)]
enum HoverModel {
    Lines {
        series: Vec<HoverSeries>,
        x_kind: XKind,
        categories: Vec<String>,
    },
    Bars(Vec<BarHit>),
}

#[derive(Debug, Clone)]
pub struct ChartScene {
    pub width: f32,
    pub height: f32,
    /// The area the series are drawn in. `plot_items` are clipped to it.
    pub plot: Rect,
    /// Gridlines, below everything.
    pub grid: Vec<Item>,
    /// Lines and bars, clipped to the plot.
    pub plot_items: Vec<Item>,
    /// Axes, labels, title and legend, above everything.
    pub chrome: Vec<Item>,
    pub legend: Vec<LegendEntry>,
    hover: HoverModel,
}

const PADDING: f32 = 12.0;
const TICK_LENGTH: f32 = 4.0;
const TICK_GAP: f32 = 4.0;
const SWATCH: f32 = 10.0;
const SWATCH_GAP: f32 = 6.0;
const LEGEND_SPACING: f32 = 14.0;
const LEGEND_ROWS_BELOW: usize = 3;
const MAXIMUM_LABEL_CHARACTERS: usize = 28;
const MAXIMUM_HOVER_ENTRIES: usize = 12;
const LINE_WIDTH: f32 = 1.5;

#[derive(Clone, Copy)]
struct Sizes {
    text: f32,
    title: f32,
}

impl Sizes {
    fn new(options: &ChartOptions, width: f32, height: f32) -> Self {
        let text = match options.text_size.and_then(TextSize::scale) {
            Some(scale) => 12.0 * scale,
            None => (width.min(height * 1.6) / 50.0).clamp(11.0, 14.0),
        };
        Self {
            text,
            title: text * 1.25,
        }
    }

    fn line(size: f32) -> f32 {
        size * 1.3
    }
}

/// An axis of numbers, times or durations.
#[derive(Debug, Clone)]
struct Continuous {
    logarithmic: bool,
    low: f64,
    high: f64,
    ticks: Vec<Tick>,
}

impl Continuous {
    fn fraction(&self, value: f64) -> f64 {
        if self.logarithmic {
            let (low, high) = (self.low.log10(), self.high.log10());
            (value.log10() - low) / (high - low)
        } else {
            (value - self.low) / (self.high - self.low)
        }
    }
}

/// The axis along which the points are spread.
#[derive(Debug, Clone)]
enum CategoryAxis {
    Continuous(Continuous),
    Bands(Vec<String>),
}

impl CategoryAxis {
    fn fraction(&self, x: f64) -> f64 {
        match self {
            CategoryAxis::Continuous(axis) => axis.fraction(x),
            CategoryAxis::Bands(labels) => (x + 0.5) / labels.len().max(1) as f64,
        }
    }
}

struct Axes {
    category: CategoryAxis,
    value: Continuous,
}

#[derive(Clone, Copy)]
struct Projection {
    plot: Rect,
    /// Categories run down the screen and values across it.
    horizontal: bool,
}

impl Projection {
    fn along_categories(&self, fraction: f64) -> f32 {
        if self.horizontal {
            self.plot.top + fraction as f32 * self.plot.height
        } else {
            self.plot.left + fraction as f32 * self.plot.width
        }
    }

    fn along_values(&self, fraction: f64) -> f32 {
        if self.horizontal {
            self.plot.left + fraction as f32 * self.plot.width
        } else {
            self.plot.bottom() - fraction as f32 * self.plot.height
        }
    }

    fn point(&self, category: f64, value: f64) -> (f32, f32) {
        if self.horizontal {
            (self.along_values(value), self.along_categories(category))
        } else {
            (self.along_categories(category), self.along_values(value))
        }
    }

    fn category_length(&self) -> f32 {
        if self.horizontal {
            self.plot.height
        } else {
            self.plot.width
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stacking {
    Grouped,
    Stacked,
    Full,
}

/// What an axis is asked to look like by the options.
struct AxisOptions {
    logarithmic: bool,
    limits: Option<(Value, Value)>,
    title: Option<String>,
    show_ticks: bool,
    show_grid: bool,
}

/// The options of the axis on the screen: the horizontal one or the vertical one.
fn axis_options(options: &ChartOptions, screen_horizontal: bool) -> AxisOptions {
    let (scale, low, high, title, ticks, grid) = if screen_horizontal {
        (
            options.x_axis,
            &options.x_min,
            &options.x_max,
            &options.x_title,
            options.x_show_ticks,
            options.x_show_grid,
        )
    } else {
        (
            options.y_axis,
            &options.y_min,
            &options.y_max,
            &options.y_title,
            options.y_show_ticks,
            options.y_show_grid,
        )
    };
    AxisOptions {
        logarithmic: scale == Some(AxisScale::Log),
        limits: match (low, high) {
            (Some(low), Some(high)) => Some((low.clone(), high.clone())),
            _ => None,
        },
        title: title.clone().filter(|title| !title.is_empty()),
        show_ticks: ticks == Some(true),
        show_grid: grid != Some(false),
    }
}

/// An axis limit as a number: a number, the text of one, or for a time axis the text of a time
/// or a timespan.
fn limit_value(limit: &Value, x_kind: Option<XKind>) -> Option<f64> {
    let value = match limit {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => match x_kind {
            Some(XKind::Time) => parse_datetime_text(text).or_else(|| text.trim().parse().ok()),
            Some(XKind::Duration) => parse_timespan_text(text).or_else(|| text.trim().parse().ok()),
            _ => text.trim().parse().ok(),
        },
        _ => None,
    };
    value.filter(|value| value.is_finite())
}

/// `text` cut to fit `limit` pixels, ending in an ellipsis when it was cut.
fn truncate(text: &str, size: f32, limit: f32, measure: &dyn TextMeasure) -> String {
    let characters: Vec<char> = text.chars().take(MAXIMUM_LABEL_CHARACTERS).collect();
    let cut_already = text.chars().count() > characters.len();
    let mut keep = characters.len();
    loop {
        let mut candidate: String = characters[..keep].iter().collect();
        if cut_already || keep < characters.len() {
            candidate.push('…');
        }
        if keep == 0 || measure.width(&candidate, size) <= limit {
            return candidate;
        }
        keep -= 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LegendPlacement {
    Right,
    Bottom,
}

struct LegendPlan {
    placement: LegendPlacement,
    entries: Vec<PlannedEntry>,
    /// The room it takes beside or below the plot.
    size: (f32, f32),
}

struct PlannedEntry {
    color_index: Option<usize>,
    name: String,
    hidden: bool,
    width: f32,
    /// From the top left of the legend.
    offset: (f32, f32),
}

/// Everything the layout reads, worked out once.
struct Layout<'a> {
    data: &'a ChartData,
    options: &'a ChartOptions,
    view: &'a ViewState,
    visible: Vec<&'a Series>,
    sizes: Sizes,
    width: f32,
    height: f32,
    horizontal: bool,
    stacking: Stacking,
    measure: &'a dyn TextMeasure,
    horizontal_axis: AxisOptions,
    vertical_axis: AxisOptions,
}

impl<'a> Layout<'a> {
    fn category_options(&self) -> &AxisOptions {
        if self.horizontal {
            &self.vertical_axis
        } else {
            &self.horizontal_axis
        }
    }

    fn value_options(&self) -> &AxisOptions {
        if self.horizontal {
            &self.horizontal_axis
        } else {
            &self.vertical_axis
        }
    }

    fn text_width(&self, text: &str) -> f32 {
        self.measure.width(text, self.sizes.text)
    }

    /// The smallest and largest value to draw, before the axis rounds them. Bars start at zero.
    fn value_extent(&self) -> (f64, f64) {
        let logarithmic = self.value_options().logarithmic;
        let drawable = |value: f64| !logarithmic || value > 0.0;
        if self.stacking == Stacking::Full {
            return (0.0, 100.0);
        }
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        if self.stacking == Stacking::Stacked && !logarithmic {
            let mut positive = vec![0.0f64; self.data.categories.len().max(1)];
            let mut negative = vec![0.0f64; positive.len()];
            for series in &self.visible {
                for point in &series.points {
                    let slot = (point.x as usize).min(positive.len() - 1);
                    if point.y >= 0.0 {
                        positive[slot] += point.y;
                    } else {
                        negative[slot] += point.y;
                    }
                }
            }
            low = negative.iter().copied().fold(0.0, f64::min);
            high = positive.iter().copied().fold(0.0, f64::max);
        } else {
            for series in &self.visible {
                for point in series.points.iter().filter(|point| drawable(point.y)) {
                    low = low.min(point.y);
                    high = high.max(point.y);
                }
            }
        }
        if !low.is_finite() || !high.is_finite() {
            return if logarithmic { (1.0, 10.0) } else { (0.0, 1.0) };
        }
        if self.data.kind.is_bars() && !logarithmic {
            low = low.min(0.0);
            high = high.max(0.0);
        }
        (low, high)
    }

    fn value_axis(&self, plot: Rect) -> Continuous {
        let options = self.value_options();
        let length = if self.horizontal {
            plot.width / 80.0
        } else {
            plot.height / 45.0
        };
        let target = length.clamp(2.0, 12.0) as usize;
        let limits = options.limits.as_ref().and_then(|(low, high)| {
            let (low, high) = (limit_value(low, None)?, limit_value(high, None)?);
            let usable = low < high && (!options.logarithmic || low > 0.0);
            usable.then_some((low, high))
        });
        let (low, high) = self.value_extent();
        let mut axis = if let Some((low, high)) = limits {
            let ticks = if options.logarithmic {
                chart_scale::log_ticks(low, high)
            } else {
                let step = chart_scale::nice_step(high - low, target);
                chart_scale::linear_ticks(low, high, step)
            };
            Continuous {
                logarithmic: options.logarithmic,
                low,
                high,
                ticks,
            }
        } else if options.logarithmic {
            let low = 10f64.powf(low.log10().floor());
            let mut high = 10f64.powf(high.log10().ceil());
            if high <= low {
                high = low * 10.0;
            }
            Continuous {
                logarithmic: true,
                low,
                high,
                ticks: chart_scale::log_ticks(low, high),
            }
        } else {
            let (low, high, step) = chart_scale::nice_range(low, high, target);
            Continuous {
                logarithmic: false,
                low,
                high,
                ticks: chart_scale::linear_ticks(low, high, step),
            }
        };
        if self.stacking == Stacking::Full {
            for tick in &mut axis.ticks {
                tick.label.push('%');
            }
        }
        axis
    }

    fn category_axis(&self, plot: Rect) -> CategoryAxis {
        let x_kind = self.data.x_kind;
        if x_kind == XKind::Category {
            return CategoryAxis::Bands(self.data.categories.clone());
        }
        let options = self.category_options();
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        let drawn: &[&Series] = &self.visible;
        let all: Vec<&Series> = self.data.series.iter().collect();
        for series in if drawn.is_empty() { &all[..] } else { drawn } {
            for point in &series.points {
                low = low.min(point.x);
                high = high.max(point.x);
            }
        }
        if let Some((given_low, given_high)) = options.limits.as_ref().and_then(|(low, high)| {
            Some((
                limit_value(low, Some(x_kind))?,
                limit_value(high, Some(x_kind))?,
            ))
        }) && given_low < given_high
        {
            low = given_low;
            high = given_high;
        }
        if !low.is_finite() || !high.is_finite() {
            (low, high) = (0.0, 1.0);
        }
        if low >= high {
            let pad = match x_kind {
                XKind::Time => 12.0 * 3_600_000.0,
                XKind::Duration => 500.0,
                _ => 1.0,
            };
            (low, high) = (low - pad, low + pad);
        }
        let target = (plot.width / 90.0).clamp(2.0, 12.0) as usize;
        let logarithmic = options.logarithmic && x_kind == XKind::Number && low > 0.0;
        let ticks = match x_kind {
            XKind::Time => chart_scale::time_ticks(low, high, target),
            XKind::Duration => chart_scale::duration_ticks(low, high, target),
            _ if logarithmic => chart_scale::log_ticks(low, high),
            _ => {
                let step = chart_scale::nice_step(high - low, target);
                chart_scale::linear_ticks(low, high, step)
            }
        };
        CategoryAxis::Continuous(Continuous {
            logarithmic,
            low,
            high,
            ticks,
        })
    }

    fn axes(&self, plot: Rect) -> Axes {
        Axes {
            category: self.category_axis(plot),
            value: self.value_axis(plot),
        }
    }

    fn legend(&self) -> Option<LegendPlan> {
        let series = &self.data.series;
        let position = self.options.legend_position;
        let explicit = matches!(
            position,
            Some(LegendPosition::Right | LegendPosition::Bottom)
        );
        if position == Some(LegendPosition::None) || (series.len() < 2 && !explicit) {
            return None;
        }
        let placement = match position {
            Some(LegendPosition::Right) => LegendPlacement::Right,
            Some(LegendPosition::Bottom) => LegendPlacement::Bottom,
            _ if self.width >= 480.0 && series.len() <= 15 => LegendPlacement::Right,
            _ => LegendPlacement::Bottom,
        };
        let size = self.sizes.text;
        let row_height = Sizes::line(size) + 2.0;
        let name_limit = match placement {
            LegendPlacement::Right => (self.width * 0.28).min(220.0),
            LegendPlacement::Bottom => (self.width - 2.0 * PADDING) / 2.0,
        } - SWATCH
            - SWATCH_GAP;
        let mut entries: Vec<PlannedEntry> = series
            .iter()
            .map(|series| {
                let name = truncate(&series.name, size, name_limit, self.measure);
                PlannedEntry {
                    color_index: Some(series.color_index),
                    width: SWATCH + SWATCH_GAP + self.measure.width(&name, size),
                    name,
                    hidden: self.view.hidden.contains(&series.color_index),
                    offset: (0.0, 0.0),
                }
            })
            .collect();

        let more = |count: usize| {
            let name = format!("+{count} more");
            PlannedEntry {
                color_index: None,
                width: self.measure.width(&name, size),
                name,
                hidden: false,
                offset: (0.0, 0.0),
            }
        };
        match placement {
            LegendPlacement::Right => {
                let rows = ((self.height * 0.8 / row_height) as usize).max(3);
                if entries.len() > rows {
                    let left_out = entries.len() - (rows - 1);
                    entries.truncate(rows - 1);
                    entries.push(more(left_out));
                }
                let widest = entries.iter().map(|entry| entry.width).fold(0.0, f32::max);
                for (row, entry) in entries.iter_mut().enumerate() {
                    entry.offset = (0.0, row as f32 * row_height);
                }
                let height = entries.len() as f32 * row_height;
                Some(LegendPlan {
                    placement,
                    entries,
                    size: (widest, height),
                })
            }
            LegendPlacement::Bottom => {
                let room = self.width - 2.0 * PADDING;
                let mut placed: Vec<PlannedEntry> = Vec::new();
                let (mut x, mut row) = (0.0f32, 0usize);
                let mut widest = 0.0f32;
                let total = entries.len();
                for entry in entries {
                    if x > 0.0 && x + entry.width > room {
                        widest = widest.max(x - LEGEND_SPACING);
                        x = 0.0;
                        row += 1;
                    }
                    if row >= LEGEND_ROWS_BELOW {
                        let left_out = total - placed.len();
                        let mut last = more(left_out);
                        if x + last.width > room {
                            // No room on the last row: replace the entry before it.
                            if let Some(previous) = placed.pop() {
                                x = previous.offset.0;
                                last = more(left_out + 1);
                            }
                        }
                        last.offset = (x, (LEGEND_ROWS_BELOW - 1) as f32 * row_height);
                        widest = widest.max(x + last.width);
                        placed.push(last);
                        let height = LEGEND_ROWS_BELOW as f32 * row_height;
                        return Some(LegendPlan {
                            placement,
                            entries: placed,
                            size: (widest, height),
                        });
                    }
                    let mut entry = entry;
                    entry.offset = (x, row as f32 * row_height);
                    x += entry.width + LEGEND_SPACING;
                    placed.push(entry);
                }
                widest = widest.max(x - LEGEND_SPACING);
                Some(LegendPlan {
                    placement,
                    entries: placed,
                    size: (widest.max(0.0), (row + 1) as f32 * row_height),
                })
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Margins {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Margins {
    fn first_guess(legend: Option<&LegendPlan>) -> Self {
        let mut margins = Self {
            left: PADDING + 40.0,
            top: PADDING + 20.0,
            right: PADDING,
            bottom: PADDING + 30.0,
        };
        margins.reserve_legend(legend);
        margins
    }

    fn reserve_legend(&mut self, legend: Option<&LegendPlan>) {
        if let Some(legend) = legend {
            match legend.placement {
                LegendPlacement::Right => self.right = PADDING + legend.size.0 + PADDING,
                LegendPlacement::Bottom => self.bottom += legend.size.1 + 6.0,
            }
        }
    }

    /// The margins that fit the labels of `axes`.
    fn measure(layout: &Layout, axes: &Axes, legend: Option<&LegendPlan>) -> Self {
        let sizes = layout.sizes;
        let line = Sizes::line(sizes.text);
        let widest = |ticks: &[Tick]| {
            ticks
                .iter()
                .map(|tick| layout.text_width(&tick.label))
                .fold(0.0, f32::max)
        };
        let category_labels_width = match &axes.category {
            CategoryAxis::Bands(labels) => labels
                .iter()
                .map(|label| {
                    let label = truncate(label, sizes.text, layout.width * 0.3, layout.measure);
                    layout.text_width(&label)
                })
                .fold(0.0, f32::max),
            CategoryAxis::Continuous(axis) => widest(&axis.ticks),
        };
        let vertical_labels_width = if layout.horizontal {
            category_labels_width
        } else {
            widest(&axes.value.ticks)
        };
        let mut left = PADDING + vertical_labels_width + TICK_LENGTH + TICK_GAP;
        let mut right = PADDING;
        if !layout.horizontal
            && let CategoryAxis::Continuous(axis) = &axes.category
        {
            // The first and last labels are centred on their ticks and can overhang the plot.
            if let Some(first) = axis.ticks.first() {
                left = left.max(layout.text_width(&first.label) / 2.0 + 4.0);
            }
            if let Some(last) = axis.ticks.last() {
                right = right.max(layout.text_width(&last.label) / 2.0 + 4.0);
            }
        }
        let horizontal_labels = if layout.horizontal {
            widest(&axes.value.ticks) / 2.0 + 4.0
        } else {
            0.0
        };
        right = right.max(horizontal_labels);

        let mut top = PADDING;
        if layout
            .options
            .title
            .as_ref()
            .is_some_and(|title| !title.is_empty())
        {
            top += Sizes::line(sizes.title) + 6.0;
        }
        if layout.vertical_axis.title.is_some() {
            top += line + 6.0;
        }
        let mut bottom = PADDING + TICK_LENGTH + TICK_GAP + line;
        if layout.horizontal_axis.title.is_some() {
            bottom += line + 6.0;
        }
        let mut margins = Self {
            left,
            top,
            right,
            bottom,
        };
        margins.reserve_legend(legend);
        margins
    }

    fn plot(&self, width: f32, height: f32) -> Rect {
        Rect {
            left: self.left,
            top: self.top,
            width: (width - self.left - self.right).max(20.0),
            height: (height - self.top - self.bottom).max(20.0),
        }
    }
}

/// Keeps the extremes of each pixel column, which is all a line of more points than pixels
/// shows, so that a spike is not lost.
fn decimate(points: &[(f32, f32)], width: f32) -> Vec<(f32, f32)> {
    let columns = width.max(1.0) as usize;
    if points.len() <= columns * 2 {
        return points.to_vec();
    }
    let mut kept = Vec::with_capacity(columns * 4);
    let mut start = 0;
    while start < points.len() {
        let column = points[start].0.floor();
        let (mut lowest, mut highest) = (start, start);
        let mut end = start;
        while end < points.len() && points[end].0.floor() == column {
            if points[end].1 < points[lowest].1 {
                lowest = end;
            }
            if points[end].1 > points[highest].1 {
                highest = end;
            }
            end += 1;
        }
        let mut chosen = [start, lowest, highest, end - 1];
        chosen.sort_unstable();
        for (position, index) in chosen.iter().enumerate() {
            if position == 0 || chosen[position - 1] != *index {
                kept.push(points[*index]);
            }
        }
        start = end;
    }
    kept
}

impl ChartScene {
    pub fn build(
        data: &ChartData,
        options: &ChartOptions,
        view: &ViewState,
        width: f32,
        height: f32,
        measure: &dyn TextMeasure,
    ) -> Self {
        let width = width.max(80.0);
        let height = height.max(60.0);
        let layout = Layout {
            data,
            options,
            view,
            visible: data
                .series
                .iter()
                .filter(|series| !view.hidden.contains(&series.color_index))
                .collect(),
            sizes: Sizes::new(options, width, height),
            width,
            height,
            horizontal: data.kind.is_bar(),
            stacking: if data.kind.is_stacked_to_full() {
                Stacking::Full
            } else if data.kind.is_stacked() {
                Stacking::Stacked
            } else {
                Stacking::Grouped
            },
            measure,
            horizontal_axis: axis_options(options, true),
            vertical_axis: axis_options(options, false),
        };
        let legend = layout.legend();

        // The ticks depend on the size of the plot, and the size of the plot on the width of
        // the labels of the ticks.
        let mut margins = Margins::first_guess(legend.as_ref());
        for _ in 0..2 {
            let axes = layout.axes(margins.plot(width, height));
            margins = Margins::measure(&layout, &axes, legend.as_ref());
        }
        let plot = margins.plot(width, height);
        let axes = layout.axes(plot);
        let projection = Projection {
            plot,
            horizontal: layout.horizontal,
        };

        let mut scene = ChartScene {
            width,
            height,
            plot,
            grid: Vec::new(),
            plot_items: Vec::new(),
            chrome: Vec::new(),
            legend: Vec::new(),
            hover: HoverModel::Bars(Vec::new()),
        };
        scene.draw_axes(&layout, &axes, projection);
        if data.kind.is_bars() {
            scene.draw_bars(&layout, &axes, projection);
        } else {
            scene.draw_lines(&layout, &axes, projection);
        }
        if let Some(title) = options.title.as_ref().filter(|title| !title.is_empty()) {
            scene.chrome.push(Item::Text {
                text: truncate(title, layout.sizes.title, width - 2.0 * PADDING, measure),
                at: (width / 2.0, PADDING),
                size: layout.sizes.title,
                horizontal: Anchor::Middle,
                vertical: VerticalAnchor::Top,
                role: Role::Text,
                bold: true,
            });
        }
        if let Some(plan) = legend {
            scene.draw_legend(&layout, plan);
        }
        scene
    }

    fn snap(value: f32) -> f32 {
        value.floor() + 0.5
    }

    fn draw_axes(&mut self, layout: &Layout, axes: &Axes, projection: Projection) {
        let plot = self.plot;
        let text = layout.sizes.text;
        let label_gap = TICK_LENGTH + TICK_GAP;
        let value_options = layout.value_options();
        let category_options = layout.category_options();

        // The axis of the values.
        for tick in &axes.value.ticks {
            let position = projection.along_values(axes.value.fraction(tick.value));
            if !position.is_finite() {
                continue;
            }
            let position = Self::snap(position);
            if layout.horizontal {
                if value_options.show_grid {
                    self.grid.push(Item::Segment {
                        from: (position, plot.top),
                        to: (position, plot.bottom()),
                        role: Role::Grid,
                        width: 1.0,
                    });
                }
                if value_options.show_ticks {
                    self.chrome.push(Item::Segment {
                        from: (position, plot.bottom()),
                        to: (position, plot.bottom() + TICK_LENGTH),
                        role: Role::Axis,
                        width: 1.0,
                    });
                }
                self.chrome.push(Item::Text {
                    text: tick.label.clone(),
                    at: (position, plot.bottom() + label_gap),
                    size: text,
                    horizontal: Anchor::Middle,
                    vertical: VerticalAnchor::Top,
                    role: Role::MutedText,
                    bold: false,
                });
            } else {
                if value_options.show_grid {
                    self.grid.push(Item::Segment {
                        from: (plot.left, position),
                        to: (plot.right(), position),
                        role: Role::Grid,
                        width: 1.0,
                    });
                }
                if value_options.show_ticks {
                    self.chrome.push(Item::Segment {
                        from: (plot.left - TICK_LENGTH, position),
                        to: (plot.left, position),
                        role: Role::Axis,
                        width: 1.0,
                    });
                }
                self.chrome.push(Item::Text {
                    text: tick.label.clone(),
                    at: (plot.left - label_gap, position),
                    size: text,
                    horizontal: Anchor::End,
                    vertical: VerticalAnchor::Middle,
                    role: Role::MutedText,
                    bold: false,
                });
            }
        }

        // The axis the points are spread along.
        match &axes.category {
            CategoryAxis::Continuous(axis) => {
                for tick in &axis.ticks {
                    let position = projection.along_categories(axis.fraction(tick.value));
                    if !position.is_finite() {
                        continue;
                    }
                    let position = Self::snap(position);
                    if category_options.show_grid {
                        self.grid.push(Item::Segment {
                            from: (position, plot.top),
                            to: (position, plot.bottom()),
                            role: Role::Grid,
                            width: 1.0,
                        });
                    }
                    if category_options.show_ticks {
                        self.chrome.push(Item::Segment {
                            from: (position, plot.bottom()),
                            to: (position, plot.bottom() + TICK_LENGTH),
                            role: Role::Axis,
                            width: 1.0,
                        });
                    }
                    self.chrome.push(Item::Text {
                        text: tick.label.clone(),
                        at: (position, plot.bottom() + label_gap),
                        size: text,
                        horizontal: Anchor::Middle,
                        vertical: VerticalAnchor::Top,
                        role: Role::MutedText,
                        bold: false,
                    });
                }
            }
            CategoryAxis::Bands(labels) => {
                let band = projection.category_length() / labels.len().max(1) as f32;
                let widest = labels
                    .iter()
                    .map(|label| {
                        layout.text_width(&truncate(label, text, f32::MAX, layout.measure))
                    })
                    .fold(0.0, f32::max);
                // Labels are shown every so many bands once they would run into each other.
                let needed = if layout.horizontal {
                    Sizes::line(text)
                } else {
                    widest + 8.0
                };
                let stride = ((needed / band.max(1.0)).ceil() as usize).max(1);
                for (index, label) in labels.iter().enumerate().step_by(stride) {
                    let center =
                        projection.along_categories((index as f64 + 0.5) / labels.len() as f64);
                    if layout.horizontal {
                        self.chrome.push(Item::Text {
                            text: truncate(label, text, layout.width * 0.3, layout.measure),
                            at: (plot.left - label_gap, center),
                            size: text,
                            horizontal: Anchor::End,
                            vertical: VerticalAnchor::Middle,
                            role: Role::MutedText,
                            bold: false,
                        });
                    } else {
                        let limit = (band * stride as f32 - 6.0).max(24.0);
                        self.chrome.push(Item::Text {
                            text: truncate(label, text, limit, layout.measure),
                            at: (center, plot.bottom() + label_gap),
                            size: text,
                            horizontal: Anchor::Middle,
                            vertical: VerticalAnchor::Top,
                            role: Role::MutedText,
                            bold: false,
                        });
                    }
                }
            }
        }

        // A line at zero where the values cross it, then the two edges.
        if !axes.value.logarithmic && axes.value.low < 0.0 && axes.value.high > 0.0 {
            let position = Self::snap(projection.along_values(axes.value.fraction(0.0)));
            let (from, to) = if layout.horizontal {
                ((position, plot.top), (position, plot.bottom()))
            } else {
                ((plot.left, position), (plot.right(), position))
            };
            self.chrome.push(Item::Segment {
                from,
                to,
                role: Role::Axis,
                width: 1.0,
            });
        }
        self.chrome.push(Item::Segment {
            from: (Self::snap(plot.left), plot.top),
            to: (Self::snap(plot.left), plot.bottom()),
            role: Role::Axis,
            width: 1.0,
        });
        self.chrome.push(Item::Segment {
            from: (plot.left, Self::snap(plot.bottom())),
            to: (plot.right(), Self::snap(plot.bottom())),
            role: Role::Axis,
            width: 1.0,
        });

        // The titles: the horizontal axis's under its labels, the vertical axis's above it.
        if let Some(title) = &layout.horizontal_axis.title {
            self.chrome.push(Item::Text {
                text: truncate(title, text, plot.width, layout.measure),
                at: (
                    plot.left + plot.width / 2.0,
                    plot.bottom() + label_gap + Sizes::line(text) + 2.0,
                ),
                size: text,
                horizontal: Anchor::Middle,
                vertical: VerticalAnchor::Top,
                role: Role::Text,
                bold: false,
            });
        }
        if let Some(title) = &layout.vertical_axis.title {
            self.chrome.push(Item::Text {
                text: truncate(title, text, plot.width, layout.measure),
                at: (PADDING, plot.top - 6.0),
                size: text,
                horizontal: Anchor::Start,
                vertical: VerticalAnchor::Bottom,
                role: Role::Text,
                bold: false,
            });
        }
    }

    fn draw_lines(&mut self, layout: &Layout, axes: &Axes, projection: Projection) {
        let options = layout.options;
        let mut hover_series = Vec::new();
        for series in &layout.visible {
            let mut pixels: Vec<(f32, f32)> = Vec::with_capacity(series.points.len());
            let mut hover_points = Vec::with_capacity(series.points.len());
            for point in &series.points {
                let drawable = (!axes.value.logarithmic || point.y > 0.0)
                    && match &axes.category {
                        CategoryAxis::Continuous(axis) => !axis.logarithmic || point.x > 0.0,
                        CategoryAxis::Bands(_) => true,
                    };
                if !drawable {
                    continue;
                }
                let pixel = projection.point(
                    axes.category.fraction(point.x),
                    axes.value.fraction(point.y),
                );
                if !(pixel.0.is_finite() && pixel.1.is_finite()) {
                    continue;
                }
                pixels.push(pixel);
                hover_points.push(HoverPoint {
                    pixel,
                    x: point.x,
                    y: point.y,
                    row: point.row,
                });
            }
            let role = Role::Series(series.color_index);
            if pixels.len() >= 2 {
                self.plot_items.push(Item::Polyline {
                    points: decimate(&pixels, self.plot.width),
                    role,
                    width: LINE_WIDTH,
                });
            }
            let markers = options.show_markers == Some(true) && pixels.len() <= 400;
            if pixels.len() == 1 || markers {
                for pixel in &pixels {
                    self.plot_items.push(Item::Circle {
                        center: *pixel,
                        radius: 3.0,
                        role,
                    });
                }
            }
            hover_series.push(HoverSeries {
                color_index: series.color_index,
                name: series.name.clone(),
                points: hover_points,
            });
        }
        self.hover = HoverModel::Lines {
            series: hover_series,
            x_kind: layout.data.x_kind,
            categories: layout.data.categories.clone(),
        };
    }

    fn draw_bars(&mut self, layout: &Layout, axes: &Axes, projection: Projection) {
        let count = layout.data.categories.len();
        if count == 0 {
            self.hover = HoverModel::Bars(Vec::new());
            return;
        }
        let band = projection.category_length() / count as f32;
        let group = band * 0.8;
        let slots = if layout.stacking == Stacking::Grouped {
            layout.visible.len().max(1)
        } else {
            1
        };
        let bar = group / slots as f32;
        let gap = if bar > 4.0 { 1.0 } else { 0.0 };
        let baseline = if axes.value.logarithmic {
            axes.value.low
        } else {
            0.0
        };
        let (low, high) = (axes.value.low, axes.value.high);
        let clamp = |value: f64| value.clamp(low, high);

        let mut totals = vec![0.0f64; count];
        if layout.stacking == Stacking::Full {
            for series in &layout.visible {
                for point in &series.points {
                    if let Some(total) = totals.get_mut(point.x as usize) {
                        *total += point.y.abs();
                    }
                }
            }
        }
        let mut positive = vec![0.0f64; count];
        let mut negative = vec![0.0f64; count];
        let mut hits = Vec::new();
        let text_size = layout.sizes.text * 0.9;
        for (slot, series) in layout.visible.iter().enumerate() {
            for point in &series.points {
                let category = point.x as usize;
                if category >= count || (axes.value.logarithmic && point.y <= 0.0) {
                    continue;
                }
                let value = if layout.stacking == Stacking::Full && totals[category] > 0.0 {
                    point.y / totals[category] * 100.0
                } else {
                    point.y
                };
                let (from, to) = match layout.stacking {
                    Stacking::Grouped => (baseline, value),
                    Stacking::Stacked | Stacking::Full => {
                        let running = if value >= 0.0 {
                            &mut positive[category]
                        } else {
                            &mut negative[category]
                        };
                        let from = *running;
                        *running += value;
                        (from, *running)
                    }
                };
                let start = projection.along_values(axes.value.fraction(clamp(from)));
                let end = projection.along_values(axes.value.fraction(clamp(to)));
                if !(start.is_finite() && end.is_finite()) {
                    continue;
                }
                let center = projection.along_categories((category as f64 + 0.5) / count as f64);
                let offset = center - group / 2.0
                    + if layout.stacking == Stacking::Grouped {
                        slot as f32 * bar
                    } else {
                        0.0
                    };
                let (across, along) = (offset, (start.min(end), (start - end).abs().max(1.0)));
                let rect = if layout.horizontal {
                    Rect {
                        left: along.0,
                        top: across,
                        width: along.1,
                        height: (bar - gap).max(1.0),
                    }
                } else {
                    Rect {
                        left: across,
                        top: along.0,
                        width: (bar - gap).max(1.0),
                        height: along.1,
                    }
                };
                self.plot_items.push(Item::Rect {
                    rect,
                    role: Role::Series(series.color_index),
                    opacity: 1.0,
                });
                if layout.options.show_values == Some(true) && layout.stacking == Stacking::Grouped
                {
                    let label = chart_scale::format_plain(point.y);
                    let outward = point.y >= 0.0;
                    let at = match (layout.horizontal, outward) {
                        (false, true) => (rect.left + rect.width / 2.0, rect.top - 2.0),
                        (false, false) => (rect.left + rect.width / 2.0, rect.bottom() + 2.0),
                        (true, true) => (rect.right() + 4.0, rect.top + rect.height / 2.0),
                        (true, false) => (rect.left - 4.0, rect.top + rect.height / 2.0),
                    };
                    let (horizontal, vertical) = match (layout.horizontal, outward) {
                        (false, true) => (Anchor::Middle, VerticalAnchor::Bottom),
                        (false, false) => (Anchor::Middle, VerticalAnchor::Top),
                        (true, true) => (Anchor::Start, VerticalAnchor::Middle),
                        (true, false) => (Anchor::End, VerticalAnchor::Middle),
                    };
                    self.chrome.push(Item::Text {
                        text: label,
                        at,
                        size: text_size,
                        horizontal,
                        vertical,
                        role: Role::Text,
                        bold: false,
                    });
                }
                hits.push(BarHit {
                    rect,
                    color_index: series.color_index,
                    series: series.name.clone(),
                    category: layout.data.categories[category].clone(),
                    value: point.y,
                    row: point.row,
                });
            }
        }
        self.hover = HoverModel::Bars(hits);
    }

    fn draw_legend(&mut self, layout: &Layout, plan: LegendPlan) {
        let size = layout.sizes.text;
        let row_height = Sizes::line(size) + 2.0;
        let origin = match plan.placement {
            LegendPlacement::Right => (self.width - PADDING - plan.size.0, self.plot.top),
            LegendPlacement::Bottom => (
                self.plot.left + ((self.plot.width - plan.size.0) / 2.0).max(0.0),
                self.height - PADDING - plan.size.1,
            ),
        };
        for entry in plan.entries {
            let left = origin.0 + entry.offset.0;
            let top = origin.1 + entry.offset.1;
            self.legend.push(LegendEntry {
                color_index: entry.color_index,
                name: entry.name.clone(),
                hidden: entry.hidden,
                rect: Rect {
                    left,
                    top,
                    width: entry.width,
                    height: row_height,
                },
            });
            let text_left = match entry.color_index {
                Some(color_index) => {
                    self.chrome.push(Item::Rect {
                        rect: Rect {
                            left,
                            top: top + (row_height - SWATCH) / 2.0,
                            width: SWATCH,
                            height: SWATCH,
                        },
                        role: Role::Series(color_index),
                        opacity: if entry.hidden { 0.25 } else { 1.0 },
                    });
                    left + SWATCH + SWATCH_GAP
                }
                None => left,
            };
            self.chrome.push(Item::Text {
                text: entry.name,
                at: (text_left, top + row_height / 2.0),
                size,
                horizontal: Anchor::Start,
                vertical: VerticalAnchor::Middle,
                role: if entry.hidden || entry.color_index.is_none() {
                    Role::MutedText
                } else {
                    Role::Text
                },
                bold: false,
            });
        }
    }

    /// The series whose legend entry is at this point.
    pub fn legend_entry_at(&self, x: f32, y: f32) -> Option<usize> {
        self.legend
            .iter()
            .find(|entry| entry.rect.contains(x, y))
            .and_then(|entry| entry.color_index)
    }

    /// What the pointer is over, for a pointer at a position in the chart.
    pub fn hover(&self, x: f32, y: f32) -> Option<Hover> {
        if !self.plot.contains(x, y) {
            return None;
        }
        match &self.hover {
            HoverModel::Lines {
                series,
                x_kind,
                categories,
            } => Self::hover_lines(series, *x_kind, categories, x, y),
            HoverModel::Bars(hits) => hits
                .iter()
                .find(|hit| hit.rect.inflated(2.0).contains(x, y))
                .map(|hit| Hover {
                    title: hit.category.clone(),
                    entries: vec![HoverEntry {
                        color_index: hit.color_index,
                        name: hit.series.clone(),
                        value: chart_scale::format_plain(hit.value),
                    }],
                    crosshair_x: None,
                    marks: Vec::new(),
                    bar: Some(hit.rect),
                    anchor: (hit.rect.left + hit.rect.width / 2.0, hit.rect.top),
                    row: hit.row,
                }),
        }
    }

    fn hover_lines(
        series: &[HoverSeries],
        x_kind: XKind,
        categories: &[String],
        x: f32,
        y: f32,
    ) -> Option<Hover> {
        // The point nearest across, over all the series, decides the X; the others are read at it.
        let mut nearest: Option<(f32, &HoverPoint)> = None;
        for series in series {
            let after = series.points.partition_point(|point| point.pixel.0 < x);
            for index in [after.checked_sub(1), Some(after)].into_iter().flatten() {
                if let Some(point) = series.points.get(index) {
                    let distance = (point.pixel.0 - x).abs();
                    if nearest.is_none_or(|(best, _)| distance < best) {
                        nearest = Some((distance, point));
                    }
                }
            }
        }
        let (distance, anchor_point) = nearest?;
        if distance > 48.0 {
            return None;
        }
        let at = anchor_point.x;
        let mut found: Vec<(f32, &HoverSeries, &HoverPoint)> = series
            .iter()
            .filter_map(|series| {
                let index = series
                    .points
                    .binary_search_by(|point| point.x.total_cmp(&at))
                    .ok()?;
                let point = &series.points[index];
                Some(((point.pixel.1 - y).abs(), series, point))
            })
            .collect();
        if found.len() > MAXIMUM_HOVER_ENTRIES {
            found.sort_by(|a, b| a.0.total_cmp(&b.0));
            found.truncate(MAXIMUM_HOVER_ENTRIES);
        }
        let title = match x_kind {
            XKind::Time => chart_scale::format_time(at),
            XKind::Duration => chart_scale::format_duration(at),
            XKind::Number => chart_scale::format_plain(at),
            XKind::Category => categories.get(at as usize).cloned().unwrap_or_default(),
        };
        let row = found
            .first()
            .map_or(anchor_point.row, |(_, _, point)| point.row);
        Some(Hover {
            title,
            entries: found
                .iter()
                .map(|(_, series, point)| HoverEntry {
                    color_index: series.color_index,
                    name: series.name.clone(),
                    value: chart_scale::format_plain(point.y),
                })
                .collect(),
            crosshair_x: Some(anchor_point.pixel.0),
            marks: found
                .iter()
                .map(|(_, series, point)| (point.pixel.0, point.pixel.1, series.color_index))
                .collect(),
            bar: None,
            anchor: (anchor_point.pixel.0, y),
            row,
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::chart::Chart;
    use crate::result::{Cell, Table};

    fn options(value: serde_json::Value) -> ChartOptions {
        Chart::from_value(json!({"options": value}))
            .map(|chart| chart.options)
            .unwrap_or_default()
    }

    fn day(number: u32) -> Cell {
        Cell::Text(format!("2007-01-{number:02}T00:00:00Z"))
    }

    fn table(columns: &[(&str, &str)], rows: Vec<Vec<Cell>>) -> Table {
        Table {
            name: "t".into(),
            columns: columns
                .iter()
                .map(|(name, type_name)| crate::Column::new(*name, *type_name))
                .collect(),
            rows,
        }
    }

    fn two_series_over_time() -> Table {
        let mut rows = Vec::new();
        for step in 1..=10u32 {
            rows.push(vec![
                day(step),
                Cell::Text("a".into()),
                Cell::Int(i64::from(step) * 10),
            ]);
            rows.push(vec![
                day(step),
                Cell::Text("b".into()),
                Cell::Int(i64::from(step) * 5),
            ]);
        }
        table(&[("T", "datetime"), ("S", "string"), ("N", "long")], rows)
    }

    fn states() -> Table {
        table(
            &[("State", "string"), ("Hail", "long"), ("Flood", "long")],
            vec![
                vec![Cell::Text("TX".into()), Cell::Int(10), Cell::Int(20)],
                vec![Cell::Text("CA".into()), Cell::Int(30), Cell::Int(5)],
                vec![Cell::Text("NY".into()), Cell::Int(20), Cell::Int(15)],
            ],
        )
    }

    fn scene(table: &Table, options: &ChartOptions, view: &ViewState) -> (ChartData, ChartScene) {
        let data = ChartData::build(table, options).unwrap();
        let scene = ChartScene::build(&data, options, view, 640.0, 360.0, &EstimatedText);
        (data, scene)
    }

    fn rects(items: &[Item]) -> Vec<Rect> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Rect { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect()
    }

    fn texts(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn all_coordinates_are_finite(scene: &ChartScene) -> bool {
        let finite = |(x, y): (f32, f32)| x.is_finite() && y.is_finite();
        scene
            .grid
            .iter()
            .chain(&scene.plot_items)
            .chain(&scene.chrome)
            .all(|item| match item {
                Item::Segment { from, to, .. } => finite(*from) && finite(*to),
                Item::Polyline { points, .. } => points.iter().copied().all(finite),
                Item::Rect { rect, .. } => [rect.left, rect.top, rect.width, rect.height]
                    .iter()
                    .all(|value| value.is_finite()),
                Item::Circle { center, .. } => finite(*center),
                Item::Text { at, .. } => finite(*at),
            })
    }

    #[test]
    fn a_time_chart_has_a_line_for_each_series_spanning_the_plot() {
        let options = options(json!({"type": "TimeLine"}));
        let (_, scene) = scene(&two_series_over_time(), &options, &ViewState::default());
        let lines: Vec<&Vec<(f32, f32)>> = scene
            .plot_items
            .iter()
            .filter_map(|item| match item {
                Item::Polyline { points, .. } => Some(points),
                _ => None,
            })
            .collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert_eq!(line.len(), 10);
            assert!(
                (line[0].0 - scene.plot.left).abs() < 0.01,
                "starts at the left edge"
            );
            assert!(
                (line[9].0 - scene.plot.right()).abs() < 0.01,
                "ends at the right edge"
            );
            assert!(
                line.iter()
                    .all(|point| scene.plot.contains(point.0, point.1))
            );
        }
        assert!(
            lines[0][9].1 < lines[1][9].1,
            "the larger value is higher up the screen"
        );
        assert!(all_coordinates_are_finite(&scene));
    }

    #[test]
    fn the_axes_have_ticks_gridlines_and_labels() {
        let chart_options = options(json!({"type": "TimeLine"}));
        let (data, scene) = scene(
            &two_series_over_time(),
            &chart_options,
            &ViewState::default(),
        );
        let labels = texts(&scene.chrome);
        assert!(labels.iter().any(|label| label == "Jan 1"), "{labels:?}");
        assert!(labels.iter().any(|label| label == "100"), "{labels:?}");
        let horizontal_lines = scene
            .grid
            .iter()
            .filter(|item| matches!(item, Item::Segment { from, to, .. } if from.1 == to.1))
            .count();
        assert!(horizontal_lines >= 3);

        let no_grid = options(json!({"type": "TimeLine", "xShowGrid": false, "yShowGrid": false}));
        let without = ChartScene::build(
            &data,
            &no_grid,
            &ViewState::default(),
            640.0,
            360.0,
            &EstimatedText,
        );
        assert!(without.grid.is_empty());
    }

    #[test]
    fn titles_and_legend_take_room_from_the_plot() {
        let plain = options(json!({"type": "TimeLine", "legendPosition": "None"}));
        let (_, bare) = scene(&two_series_over_time(), &plain, &ViewState::default());
        let titled = options(json!({
            "type": "TimeLine", "title": "Storms", "xTitle": "when", "yTitle": "count"
        }));
        let (_, full) = scene(&two_series_over_time(), &titled, &ViewState::default());
        assert!(full.plot.height < bare.plot.height);
        assert!(
            full.plot.width < bare.plot.width,
            "the legend is at the right"
        );
        let labels = texts(&full.chrome);
        for expected in ["Storms", "when", "count", "a", "b"] {
            assert!(
                labels.iter().any(|label| label == expected),
                "{expected}: {labels:?}"
            );
        }
    }

    #[test]
    fn a_series_switched_off_leaves_the_plot_and_the_scale() {
        let options = options(json!({"type": "TimeLine"}));
        let (data, all) = scene(&two_series_over_time(), &options, &ViewState::default());
        let mut view = ViewState::default();
        view.toggle(data.series[0].color_index);
        let (_, without) = scene(&two_series_over_time(), &options, &view);
        let line_count = |scene: &ChartScene| {
            scene
                .plot_items
                .iter()
                .filter(|item| matches!(item, Item::Polyline { .. }))
                .count()
        };
        assert_eq!(line_count(&all), 2);
        assert_eq!(line_count(&without), 1);
        assert!(texts(&all.chrome).contains(&"100".to_string()));
        assert!(
            !texts(&without.chrome).contains(&"100".to_string()),
            "the axis fits what is left"
        );
        assert!(without.legend.iter().any(|entry| entry.hidden));
        view.toggle(data.series[0].color_index);
        assert!(view.hidden.is_empty());
    }

    #[test]
    fn the_legend_sits_in_a_place_a_click_can_find() {
        let options = options(json!({"type": "TimeLine"}));
        let (data, scene) = scene(&two_series_over_time(), &options, &ViewState::default());
        assert_eq!(scene.legend.len(), 2);
        let entry = &scene.legend[1];
        assert!(
            entry.rect.left > scene.plot.right(),
            "to the right of the plot"
        );
        let (x, y) = (entry.rect.left + 2.0, entry.rect.top + 2.0);
        assert_eq!(
            scene.legend_entry_at(x, y),
            Some(data.series[1].color_index)
        );
        assert_eq!(scene.legend_entry_at(1.0, 1.0), None);
    }

    #[test]
    fn a_single_series_has_no_legend_unless_asked_for() {
        let one = table(
            &[("T", "datetime"), ("N", "long")],
            vec![vec![day(1), Cell::Int(1)], vec![day(2), Cell::Int(2)]],
        );
        let (_, scene_without) = scene(
            &one,
            &options(json!({"type": "Line"})),
            &ViewState::default(),
        );
        assert!(scene_without.legend.is_empty());
        let (_, scene_with) = scene(
            &one,
            &options(json!({"type": "Line", "legendPosition": "Bottom"})),
            &ViewState::default(),
        );
        assert_eq!(scene_with.legend.len(), 1);
    }

    #[test]
    fn many_series_get_a_legend_that_says_how_many_more() {
        let mut rows = Vec::new();
        for series in 0..40 {
            for step in 1..=3u32 {
                rows.push(vec![
                    day(step),
                    Cell::Text(format!("series {series}")),
                    Cell::Int(i64::from(step) + series),
                ]);
            }
        }
        let table = table(&[("T", "datetime"), ("S", "string"), ("N", "long")], rows);
        let (_, scene) = scene(
            &table,
            &options(json!({"type": "Line"})),
            &ViewState::default(),
        );
        let last = scene.legend.last().unwrap();
        assert_eq!(last.color_index, None);
        assert!(
            last.name.starts_with('+') && last.name.ends_with("more"),
            "{}",
            last.name
        );
        assert!(
            scene
                .legend
                .iter()
                .all(|entry| entry.rect.bottom() <= scene.height)
        );

        let below = options(json!({"type": "Line", "legendPosition": "Bottom"}));
        let (_, scene) = scene_with_width(&table, &below, 400.0);
        assert!(
            scene
                .legend
                .iter()
                .all(|entry| entry.rect.bottom() <= scene.height)
        );
        assert!(
            scene
                .legend
                .iter()
                .all(|entry| entry.rect.right() <= scene.width)
        );
        assert!(scene.legend.last().unwrap().color_index.is_none());
    }

    fn scene_with_width(
        table: &Table,
        options: &ChartOptions,
        width: f32,
    ) -> (ChartData, ChartScene) {
        let data = ChartData::build(table, options).unwrap();
        let scene = ChartScene::build(
            &data,
            options,
            &ViewState::default(),
            width,
            300.0,
            &EstimatedText,
        );
        (data, scene)
    }

    #[test]
    fn grouped_columns_stand_on_the_zero_line_in_proportion() {
        let options = options(json!({"type": "Column"}));
        let (_, scene) = scene(&states(), &options, &ViewState::default());
        // The bars come series by series: Hail in TX, CA, NY, then Flood in the same order.
        let bars = rects(&scene.plot_items);
        assert_eq!(bars.len(), 6, "two series over three categories");
        let bottom = scene.plot.bottom();
        assert!(bars.iter().all(|bar| (bar.bottom() - bottom).abs() < 1.0));
        assert!(
            (bars[1].height / bars[0].height - 3.0).abs() < 0.05,
            "30 against 10"
        );
        assert!(
            bars[0].right() <= bars[3].left + 1.0,
            "the two bars of TX are side by side"
        );
        assert!(
            bars[0].left < bars[1].left && bars[1].left < bars[2].left,
            "in order"
        );
        let category_labels = texts(&scene.chrome)
            .iter()
            .filter(|text| ["TX", "CA", "NY"].contains(&text.as_str()))
            .count();
        assert_eq!(category_labels, 3);
    }

    #[test]
    fn stacked_columns_add_up_and_a_hundred_percent_fills_the_plot() {
        let (_, scene) = scene(
            &states(),
            &options(json!({"type": "ColumnStacked"})),
            &ViewState::default(),
        );
        let bars = rects(&scene.plot_items);
        assert_eq!(bars.len(), 6);
        // TX stacks Flood (20) on Hail (10), in one column.
        assert!((bars[0].left - bars[3].left).abs() < 0.01);
        assert!(
            (bars[0].top - bars[3].bottom()).abs() < 1.0,
            "Flood sits on Hail"
        );

        let (_, full) = scene_of(&states(), json!({"type": "ColumnStacked100"}));
        let bars = rects(&full.plot_items);
        let tops: Vec<f32> = (0..3)
            .map(|index| bars[index].top.min(bars[index + 3].top))
            .collect();
        assert!(
            tops.iter().all(|top| (top - full.plot.top).abs() < 1.0),
            "{tops:?}"
        );
        assert!(texts(&full.chrome).contains(&"100%".to_string()));
    }

    fn scene_of(table: &Table, value: serde_json::Value) -> (ChartData, ChartScene) {
        scene(table, &options(value), &ViewState::default())
    }

    #[test]
    fn horizontal_bars_grow_from_the_left_with_categories_down_the_side() {
        let (_, scene) = scene_of(&states(), json!({"type": "Bar"}));
        let bars = rects(&scene.plot_items);
        assert_eq!(bars.len(), 6);
        assert!(
            bars.iter()
                .all(|bar| (bar.left - scene.plot.left).abs() < 1.0)
        );
        // Hail in CA is 30, the longest, and TX comes before CA, higher up.
        let longest = bars.iter().map(|bar| bar.width).fold(0.0, f32::max);
        assert!((bars[1].width - longest).abs() < 0.01);
        assert!(bars[0].top < bars[1].top);
        let labels: Vec<_> = scene
            .chrome
            .iter()
            .filter_map(|item| match item {
                Item::Text {
                    text,
                    at,
                    horizontal: Anchor::End,
                    ..
                } if ["TX", "CA", "NY"].contains(&text.as_str()) => Some((text.clone(), at.0)),
                _ => None,
            })
            .collect();
        assert_eq!(labels.len(), 3);
        assert!(
            labels.iter().all(|(_, x)| *x < scene.plot.left),
            "the labels are left of the plot"
        );
    }

    #[test]
    fn values_can_be_written_on_the_bars() {
        let (_, scene) = scene_of(&states(), json!({"type": "Column", "showValues": true}));
        let labels = texts(&scene.chrome);
        for expected in ["10", "30", "20", "5", "15"] {
            assert!(
                labels.contains(&expected.to_string()),
                "{expected}: {labels:?}"
            );
        }
    }

    #[test]
    fn negative_values_hang_below_the_zero_line() {
        let table = table(
            &[("K", "string"), ("V", "long")],
            vec![
                vec![Cell::Text("up".into()), Cell::Int(10)],
                vec![Cell::Text("down".into()), Cell::Int(-10)],
            ],
        );
        let (_, scene) = scene_of(&table, json!({"type": "Column"}));
        let bars = rects(&scene.plot_items);
        assert!(
            (bars[0].bottom() - bars[1].top).abs() < 1.0,
            "they meet at zero"
        );
        assert!(bars[0].top < bars[1].top);
    }

    #[test]
    fn a_log_axis_leaves_out_what_cannot_be_drawn() {
        let table = table(
            &[("T", "datetime"), ("N", "long")],
            vec![
                vec![day(1), Cell::Int(0)],
                vec![day(2), Cell::Int(10)],
                vec![day(3), Cell::Int(1000)],
                vec![day(4), Cell::Int(-5)],
            ],
        );
        let (_, scene) = scene_of(&table, json!({"type": "Line", "yAxis": "Log"}));
        let points = scene.plot_items.iter().find_map(|item| match item {
            Item::Polyline { points, .. } => Some(points.len()),
            _ => None,
        });
        assert_eq!(points, Some(2));
        let labels = texts(&scene.chrome);
        assert!(
            labels.contains(&"10".to_string()) && labels.contains(&"1,000".to_string()),
            "{labels:?}"
        );
        assert!(all_coordinates_are_finite(&scene));
    }

    #[test]
    fn limits_in_the_options_set_the_axis() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 0, "yMax": 1000, "xMin": "2007-01-01T00:00:00Z", "xMax": "2007-01-21T00:00:00Z"}),
        );
        let labels = texts(&scene.chrome);
        assert!(labels.contains(&"1000".to_string()), "{labels:?}");
        let end = scene.plot_items.iter().find_map(|item| match item {
            Item::Polyline { points, .. } => points.last().copied(),
            _ => None,
        });
        let end = end.unwrap();
        assert!(
            end.0 < scene.plot.left + scene.plot.width * 0.5,
            "ten days of twenty"
        );
        assert!(
            end.1 > scene.plot.top + scene.plot.height * 0.8,
            "100 of 1000"
        );

        let (_, one_sided) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMax": 1000}),
        );
        assert!(
            !texts(&one_sided.chrome).contains(&"1000".to_string()),
            "both limits are needed"
        );
    }

    #[test]
    fn a_long_series_is_thinned_without_losing_a_spike() {
        let mut rows = Vec::new();
        for step in 0..20_000i64 {
            let value = if step == 7_777 { 1_000_000 } else { step % 100 };
            rows.push(vec![
                Cell::Text(format!("2007-01-01T00:00:00.{:03}Z", 0)),
                Cell::Int(step),
                Cell::Int(value),
            ]);
        }
        let table = table(&[("T", "datetime"), ("X", "long"), ("Y", "long")], rows);
        let (_, scene) = scene_of(
            &table,
            json!({"type": "Line", "xColumn": "X", "yColumns": ["Y"]}),
        );
        let line = scene.plot_items.iter().find_map(|item| match item {
            Item::Polyline { points, .. } => Some(points.clone()),
            _ => None,
        });
        let line = line.unwrap();
        assert!(line.len() < 20_000 / 4, "{} points", line.len());
        let highest = line
            .iter()
            .map(|point| point.1)
            .fold(f32::INFINITY, f32::min);
        assert!(
            (highest - scene.plot.top).abs() < 2.0,
            "the spike reaches the top of the plot"
        );
    }

    #[test]
    fn crowded_category_labels_are_thinned_not_overprinted() {
        let rows = (0..60)
            .map(|index| {
                vec![
                    Cell::Text(format!("category number {index}")),
                    Cell::Int(index),
                ]
            })
            .collect();
        let table = table(&[("K", "string"), ("V", "long")], rows);
        let (_, scene) = scene_of(&table, json!({"type": "Column"}));
        let shown = texts(&scene.chrome)
            .iter()
            .filter(|text| text.starts_with("categ"))
            .count();
        assert!(shown > 3 && shown < 30, "{shown} labels");
    }

    #[test]
    fn hovering_a_line_chart_reads_every_series_at_the_nearest_x() {
        let options = options(json!({"type": "TimeLine"}));
        let (data, scene) = scene(&two_series_over_time(), &options, &ViewState::default());
        let target = scene
            .plot_items
            .iter()
            .find_map(|item| match item {
                Item::Polyline { points, .. } => Some(points[4]),
                _ => None,
            })
            .unwrap();
        let hover = scene.hover(target.0 + 3.0, target.1).unwrap();
        assert_eq!(hover.title, "2007-01-05T00:00:00Z");
        assert_eq!(hover.entries.len(), 2);
        assert_eq!(hover.entries[0].name, "a");
        assert_eq!(hover.entries[0].value, "50");
        assert_eq!(hover.entries[1].value, "25");
        assert_eq!(hover.crosshair_x, Some(target.0));
        assert_eq!(hover.marks.len(), 2);
        assert_eq!(hover.marks[0].2, data.series[0].color_index);
        assert!(scene.hover(1.0, 1.0).is_none(), "outside the plot");
    }

    #[test]
    fn hovering_a_bar_names_the_bar() {
        let (_, scene) = scene_of(&states(), json!({"type": "Column"}));
        let bars = rects(&scene.plot_items);
        let bar = bars[1];
        let hover = scene
            .hover(bar.left + bar.width / 2.0, bar.top + bar.height / 2.0)
            .unwrap();
        assert_eq!(hover.title, "CA");
        assert_eq!(hover.entries[0].name, "Hail");
        assert_eq!(hover.entries[0].value, "30");
        assert_eq!(hover.bar, Some(bar));
        assert_eq!(hover.row, 1);
        assert!(
            scene
                .hover(scene.plot.left + 1.0, scene.plot.top + 1.0)
                .is_none()
        );
    }

    #[test]
    fn hover_entries_are_capped_but_stay_the_nearest() {
        let mut rows = Vec::new();
        for series in 0..30i64 {
            for step in 1..=3u32 {
                rows.push(vec![
                    day(step),
                    Cell::Text(format!("s{series}")),
                    Cell::Int(series * 10 + i64::from(step)),
                ]);
            }
        }
        let table = table(&[("T", "datetime"), ("S", "string"), ("N", "long")], rows);
        let (_, scene) = scene_of(&table, json!({"type": "Line"}));
        let hover = scene
            .hover(
                scene.plot.left + 1.0,
                scene.plot.top + scene.plot.height / 2.0,
            )
            .unwrap();
        assert_eq!(hover.entries.len(), MAXIMUM_HOVER_ENTRIES);
    }

    #[test]
    fn every_chart_type_lays_out_with_finite_coordinates_at_any_size() {
        for kind in [
            "Line",
            "TimeLine",
            "Bar",
            "BarStacked",
            "BarStacked100",
            "Column",
            "ColumnStacked",
            "ColumnStacked100",
        ] {
            for (width, height) in [(30.0, 20.0), (200.0, 120.0), (900.0, 500.0)] {
                for table in [two_series_over_time(), states()] {
                    let options = options(
                        json!({"type": kind, "title": "T", "xTitle": "x", "yTitle": "y", "showValues": true}),
                    );
                    let Ok(data) = ChartData::build(&table, &options) else {
                        continue;
                    };
                    let scene = ChartScene::build(
                        &data,
                        &options,
                        &ViewState::default(),
                        width,
                        height,
                        &EstimatedText,
                    );
                    assert!(
                        all_coordinates_are_finite(&scene),
                        "{kind} at {width}x{height}"
                    );
                    assert!(scene.plot.width > 0.0 && scene.plot.height > 0.0);
                }
            }
        }
    }

    #[test]
    fn nothing_visible_still_draws_axes() {
        let options = options(json!({"type": "TimeLine"}));
        let data = ChartData::build(&two_series_over_time(), &options).unwrap();
        let mut view = ViewState::default();
        for series in &data.series {
            view.toggle(series.color_index);
        }
        let scene = ChartScene::build(&data, &options, &view, 640.0, 360.0, &EstimatedText);
        assert!(scene.plot_items.is_empty());
        assert!(!scene.chrome.is_empty());
        assert!(all_coordinates_are_finite(&scene));
        assert!(
            scene
                .hover(scene.plot.left + 5.0, scene.plot.top + 5.0)
                .is_none()
        );
    }

    #[test]
    fn text_is_cut_to_fit_and_ends_in_an_ellipsis() {
        let measure = EstimatedText;
        assert_eq!(truncate("short", 10.0, 100.0, &measure), "short");
        let cut = truncate("a long label that does not fit", 10.0, 40.0, &measure);
        assert!(
            cut.ends_with('…') && measure.width(&cut, 10.0) <= 40.0,
            "{cut}"
        );
        assert_eq!(truncate("x", 10.0, 1.0, &measure), "…");
        let long = "y".repeat(100);
        assert_eq!(
            truncate(&long, 10.0, 10_000.0, &measure).chars().count(),
            MAXIMUM_LABEL_CHARACTERS + 1
        );
    }
}
