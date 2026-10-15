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

use crate::chart::{
    AxisScale, ChartOptions, LegendPosition, LineColor, LineOrientation, ReferenceLine, TextSize,
};
use crate::chart_data::{ChartData, Series, XKind, limit_number};
use crate::chart_scale::{self, Tick};

/// How wide text is when drawn at a size, in pixels.
pub trait TextMeasure {
    fn width(&self, text: &str, size: f32) -> f32;
}

/// A guess at text width, for tests and for writing SVG where no font is at hand. Capitals and
/// wide letters count for more than narrow ones, which is close enough to size a margin.
pub struct EstimatedText;

impl TextMeasure for EstimatedText {
    fn width(&self, text: &str, size: f32) -> f32 {
        let ems: f32 = text
            .chars()
            .map(|character| match character {
                ' ' => 0.28,
                'i' | 'l' | 'j' | 't' | 'f' | 'I' | '.' | ',' | ':' | ';' | '|' | '!' | '\''
                | '`' | '(' | ')' | '[' | ']' | '-' | '/' | '\\' => 0.3,
                'm' | 'w' => 0.8,
                'M' | 'W' | '@' | '%' => 0.95,
                'A'..='Z' => 0.68,
                '0'..='9' => 0.56,
                character if character.is_ascii() => 0.54,
                character if (character as u32) >= 0x2E80 => 1.0,
                _ => 0.6,
            })
            .sum();
        ems * size
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
    /// The colour of a line the user drew across the chart.
    Reference(LineColor),
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
    /// A segment drawn in dashes, as a reference line is by default.
    DashedSegment {
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
    /// What the layout was drawn from, to say what the pointer is over in the data.
    pointer: PointerModel,
}

#[derive(Debug, Clone)]
struct PointerModel {
    axes: Axes,
    projection: Projection,
    x_kind: XKind,
    horizontal_chart: bool,
}

/// A value of an axis, as text that can be typed into a line's value and read back.
#[derive(Debug, Clone, PartialEq)]
pub struct AxisValue {
    pub text: String,
}

/// The values of the two axes at a point of the plot. An axis of categories gives the label of
/// the category the point is in.
#[derive(Debug, Clone, PartialEq)]
pub struct PointerPosition {
    /// On the horizontal axis.
    pub x: Option<AxisValue>,
    /// On the vertical axis.
    pub y: Option<AxisValue>,
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
/// How far a line chart keeps its first and last points from the ends of its category axis.
const DATA_INSET: f32 = 8.0;

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
    /// The value a fraction of the way along the axis stands for: the inverse of `fraction`.
    fn value_at(&self, fraction: f64) -> f64 {
        if self.logarithmic {
            let (low, high) = (self.low.log10(), self.high.log10());
            10f64.powf(low + fraction * (high - low))
        } else {
            self.low + fraction * (self.high - self.low)
        }
    }

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

#[derive(Debug, Clone)]
struct Axes {
    category: CategoryAxis,
    value: Continuous,
}

#[derive(Debug, Clone, Copy)]
struct Projection {
    plot: Rect,
    /// Categories run down the screen and values across it.
    horizontal: bool,
    /// How far in from the ends of the plot the first and last position along the categories
    /// are. The values are not inset: a line may touch the top, and the base is the axis.
    category_inset: f32,
}

impl Projection {
    fn along_categories(&self, fraction: f64) -> f32 {
        let inset = self.category_inset;
        if self.horizontal {
            self.plot.top + inset + fraction as f32 * (self.plot.height - 2.0 * inset)
        } else {
            self.plot.left + inset + fraction as f32 * (self.plot.width - 2.0 * inset)
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
    /// Either end can be given alone; the other is then worked out from the data.
    minimum: Option<Value>,
    maximum: Option<Value>,
    title: Option<String>,
    show_ticks: bool,
    show_grid: bool,
}

/// The options of the axis on the screen: the horizontal one or the vertical one. Unless the
/// options say, the gridlines go across the axis of the values and the tick marks are on the axis
/// the data runs along.
fn axis_options(
    options: &ChartOptions,
    screen_horizontal: bool,
    is_value_axis: bool,
) -> AxisOptions {
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
        minimum: low.clone(),
        maximum: high.clone(),
        title: title.clone().filter(|title| !title.is_empty()),
        show_ticks: ticks.unwrap_or(!is_value_axis),
        show_grid: grid.unwrap_or(is_value_axis),
    }
}

/// A value of an axis as text for a line's value: rounded to what the pointer can tell apart on
/// this axis, and in a form `limit_number` reads back.
fn axis_value_text(value: f64, axis: &Continuous, kind: Option<XKind>) -> String {
    let span = (axis.high - axis.low).abs().max(f64::MIN_POSITIVE);
    match kind {
        Some(XKind::Time) => {
            const SECOND: f64 = 1000.0;
            const MINUTE: f64 = 60.0 * SECOND;
            const HOUR: f64 = 60.0 * MINUTE;
            const DAY: f64 = 24.0 * HOUR;
            let unit = if span < 2.0 * MINUTE {
                SECOND
            } else if span < 2.0 * HOUR {
                MINUTE
            } else if span < 2.0 * DAY {
                HOUR
            } else {
                DAY
            };
            chart_scale::format_time((value / unit).round() * unit)
        }
        Some(XKind::Duration) => chart_scale::format_timespan(value.round()),
        _ if axis.logarithmic => format!("{value:.3e}")
            .parse::<f64>()
            .map_or_else(|_| value.to_string(), |rounded| rounded.to_string()),
        _ => {
            // Three digits at the scale of the axis: 5,000 on an axis of 0 to 10,000.
            let step = 10f64.powf(span.log10().floor() - 2.0);
            let rounded = (value / step).round() * step;
            let decimals = (-step.log10()).ceil().max(0.0) as usize;
            let text = format!("{rounded:.decimals$}");
            if text.contains('.') {
                text.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                text
            }
        }
    }
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
    fn lines(&self) -> &[ReferenceLine] {
        self.options.reference_lines.as_deref().unwrap_or_default()
    }

    /// Whether a reference line is at a value of the axis of the values (else of the category
    /// axis). A horizontal line is at a value of the vertical axis, which holds the values unless
    /// the chart is of horizontal bars.
    fn on_value_axis(&self, line: &ReferenceLine) -> bool {
        (line.orientation == LineOrientation::Horizontal) != self.horizontal
    }

    /// The smallest and largest value to draw, with the lines the user drew in view too.
    fn value_extent(&self) -> (f64, f64) {
        let logarithmic = self.value_options().logarithmic;
        let (mut low, mut high) = self.data_value_extent();
        for line in self.lines().iter().filter(|line| self.on_value_axis(line)) {
            if let Some(value) =
                limit_number(&line.value, None).filter(|value| !logarithmic || *value > 0.0)
            {
                low = low.min(value);
                high = high.max(value);
            }
        }
        (low, high)
    }

    fn data_value_extent(&self) -> (f64, f64) {
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
            // Room for the value written above the longest bar.
            if self.options.show_values == Some(true) && self.stacking == Stacking::Grouped {
                let room = (high - low) * 0.08;
                if high > 0.0 {
                    high += room;
                }
                if low < 0.0 {
                    low -= room;
                }
            }
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
        let given_low = options
            .minimum
            .as_ref()
            .and_then(|limit| limit_number(limit, None));
        let given_high = options
            .maximum
            .as_ref()
            .and_then(|limit| limit_number(limit, None));
        let (data_low, data_high) = self.value_extent();
        // What the axis would be without limits: rounded out to whole steps, or to decades.
        let (auto_low, auto_high) = if options.logarithmic {
            let low = 10f64.powf(data_low.log10().floor());
            let high = 10f64.powf(data_high.log10().ceil());
            (low, if high <= low { low * 10.0 } else { high })
        } else {
            let (low, high, _) = chart_scale::nice_range(data_low, data_high, target);
            (low, high)
        };
        let (low, high) = (
            given_low.unwrap_or(auto_low),
            given_high.unwrap_or(auto_high),
        );
        let limited = (given_low.is_some() || given_high.is_some())
            && low < high
            && (!options.logarithmic || low > 0.0);
        let mut axis = if options.logarithmic {
            let (low, high) = if limited {
                (low, high)
            } else {
                (auto_low, auto_high)
            };
            Continuous {
                logarithmic: true,
                low,
                high,
                ticks: chart_scale::log_ticks(low, high),
            }
        } else if limited {
            let step = chart_scale::nice_step(high - low, target);
            Continuous {
                logarithmic: false,
                low,
                high,
                ticks: chart_scale::linear_ticks(low, high, step),
            }
        } else {
            let (low, high, step) = chart_scale::nice_range(data_low, data_high, target);
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
        // A line the user drew is in view, as far as the limits they set allow.
        for line in self.lines().iter().filter(|line| !self.on_value_axis(line)) {
            if let Some(value) = limit_number(&line.value, Some(x_kind)) {
                low = low.min(value);
                high = high.max(value);
            }
        }
        let given_low = options
            .minimum
            .as_ref()
            .and_then(|limit| limit_number(limit, Some(x_kind)));
        let given_high = options
            .maximum
            .as_ref()
            .and_then(|limit| limit_number(limit, Some(x_kind)));
        let (limited_low, limited_high) = (given_low.unwrap_or(low), given_high.unwrap_or(high));
        if limited_low < limited_high {
            low = limited_low;
            high = limited_high;
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
            horizontal_axis: axis_options(options, true, data.kind.is_bar()),
            vertical_axis: axis_options(options, false, !data.kind.is_bar()),
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
        // A line is kept in from the ends of its axis, so that the marker of a point at an end is
        // not cut off; bars fill their bands.
        let projection = Projection {
            plot,
            horizontal: layout.horizontal,
            category_inset: if !data.kind.is_bars()
                && matches!(axes.category, CategoryAxis::Continuous(_))
            {
                DATA_INSET
            } else {
                0.0
            },
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
            pointer: PointerModel {
                axes: axes.clone(),
                projection,
                x_kind: data.x_kind,
                horizontal_chart: layout.horizontal,
            },
        };
        scene.draw_axes(&layout, &axes, projection);
        scene.draw_reference_lines(&layout, &axes, projection);
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

        // A line at zero where the values cross it, then the one axis the data stands on or runs
        // along; the other edge is left to the gridlines and the labels.
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
        let (from, to) = if layout.horizontal {
            (
                (Self::snap(plot.left), plot.top),
                (Self::snap(plot.left), plot.bottom()),
            )
        } else {
            (
                (plot.left, Self::snap(plot.bottom())),
                (plot.right(), Self::snap(plot.bottom())),
            )
        };
        self.chrome.push(Item::Segment {
            from,
            to,
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

    /// The lines the user drew, under the series so that the data stays in front, with their
    /// labels above everything.
    fn draw_reference_lines(&mut self, layout: &Layout, axes: &Axes, projection: Projection) {
        let plot = self.plot;
        let size = layout.sizes.text * 0.9;
        for line in layout.lines() {
            // The vertical axis positions a horizontal line, and the horizontal one a vertical line.
            let on_vertical_axis = line.orientation == LineOrientation::Horizontal;
            let position = if layout.on_value_axis(line) {
                let value = limit_number(&line.value, None);
                value
                    .filter(|value| !axes.value.logarithmic || *value > 0.0)
                    .map(|value| projection.along_values(axes.value.fraction(value)))
            } else {
                match &axes.category {
                    CategoryAxis::Continuous(axis) => {
                        limit_number(&line.value, Some(layout.data.x_kind))
                            .filter(|value| !axis.logarithmic || *value > 0.0)
                            .map(|value| projection.along_categories(axis.fraction(value)))
                    }
                    CategoryAxis::Bands(labels) => {
                        let text = line.value_text();
                        labels.iter().position(|label| *label == text).map(|index| {
                            projection.along_categories((index as f64 + 0.5) / labels.len() as f64)
                        })
                    }
                }
            };
            let Some(position) = position.filter(|position| position.is_finite()) else {
                continue;
            };
            let position = Self::snap(position);
            let (from, to) = if on_vertical_axis {
                ((plot.left, position), (plot.right(), position))
            } else {
                ((position, plot.top), (position, plot.bottom()))
            };
            let role = Role::Reference(line.color.unwrap_or(LineColor::Grey));
            self.plot_items.push(if line.is_dashed() {
                Item::DashedSegment {
                    from,
                    to,
                    role,
                    width: 1.5,
                }
            } else {
                Item::Segment {
                    from,
                    to,
                    role,
                    width: 1.5,
                }
            });

            let Some(label) = line.label.as_ref().filter(|label| !label.is_empty()) else {
                continue;
            };
            let inside = if on_vertical_axis {
                position >= plot.top && position <= plot.bottom()
            } else {
                position >= plot.left && position <= plot.right()
            };
            if !inside {
                continue;
            }
            let label = truncate(label, size, plot.width * 0.5, layout.measure);
            let width = layout.measure.width(&label, size);
            let (at, horizontal, vertical) = if on_vertical_axis {
                // At the right end, above the line, or below it when there is no room above.
                if position - Sizes::line(size) >= plot.top {
                    (
                        (plot.right() - 4.0, position - 3.0),
                        Anchor::End,
                        VerticalAnchor::Bottom,
                    )
                } else {
                    (
                        (plot.right() - 4.0, position + 3.0),
                        Anchor::End,
                        VerticalAnchor::Top,
                    )
                }
            } else if position + 4.0 + width <= plot.right() {
                // At the top, to the right of the line, or to its left when there is no room.
                (
                    (position + 4.0, plot.top + 3.0),
                    Anchor::Start,
                    VerticalAnchor::Top,
                )
            } else {
                (
                    (position - 4.0, plot.top + 3.0),
                    Anchor::End,
                    VerticalAnchor::Top,
                )
            };
            self.chrome.push(Item::Text {
                text: label,
                at,
                size,
                horizontal,
                vertical,
                role,
                bold: false,
            });
        }
    }

    /// The values of the axes at a point of the chart, for a line to be put there. `None` outside
    /// the plot.
    pub fn pointer_position(&self, x: f32, y: f32) -> Option<PointerPosition> {
        if !self.plot.contains(x, y) {
            return None;
        }
        let model = &self.pointer;
        let projection = model.projection;
        let plot = self.plot;
        // The fraction of the way along an axis, from its start, as the projection spreads it.
        let along_categories = |position: f32, start: f32, length: f32| {
            let inset = projection.category_inset;
            f64::from((position - start - inset) / (length - 2.0 * inset))
        };
        let (category_position, category_start, category_length) = if model.horizontal_chart {
            (y, plot.top, plot.height)
        } else {
            (x, plot.left, plot.width)
        };
        let category = match &model.axes.category {
            CategoryAxis::Continuous(axis) => {
                let fraction = along_categories(category_position, category_start, category_length);
                let kind = Some(model.x_kind);
                Some(AxisValue {
                    text: axis_value_text(axis.value_at(fraction), axis, kind),
                })
            }
            CategoryAxis::Bands(labels) => {
                let fraction = f64::from((category_position - category_start) / category_length);
                let index = (fraction * labels.len() as f64).floor().max(0.0) as usize;
                labels.get(index).map(|label| AxisValue {
                    text: label.clone(),
                })
            }
        };
        let value_fraction = if model.horizontal_chart {
            f64::from((x - plot.left) / plot.width)
        } else {
            f64::from((plot.bottom() - y) / plot.height)
        };
        let value = AxisValue {
            text: axis_value_text(
                model.axes.value.value_at(value_fraction),
                &model.axes.value,
                None,
            ),
        };
        // The category axis is the vertical one for horizontal bars.
        if model.horizontal_chart {
            Some(PointerPosition {
                x: Some(value),
                y: category,
            })
        } else {
            Some(PointerPosition {
                x: category,
                y: Some(value),
            })
        }
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
        // The point nearest across, over all the series, decides the X; the others are read at
        // it. There is no limit to how far away it may be, so a sparse series still answers.
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
        let (_, anchor_point) = nearest?;
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

/// How a chart is written as SVG: its style, as CSS colour text, and its font.
#[derive(Debug, Clone)]
pub struct SvgStyle {
    /// The fill behind the chart, or none to leave it transparent.
    pub background: Option<String>,
    pub text: String,
    pub muted: String,
    pub grid: String,
    pub axis: String,
    /// By `color_index`, which wraps around when there are more series than style.
    pub series: Vec<String>,
    /// The colours of reference lines, in the order of `LineColor::ALL`.
    pub reference: Vec<String>,
    /// A CSS font family list. The layout measured text with a guess, so a font of about the
    /// width of Helvetica is the one the picture was laid out for.
    pub font_family: String,
}

impl SvgStyle {
    pub fn light() -> Self {
        Self {
            background: Some("#ffffff".into()),
            text: "#1f2328".into(),
            muted: "#59636e".into(),
            grid: "#e6e9ec".into(),
            axis: "#9aa3ad".into(),
            series: [
                "#3b82c4", "#d9822b", "#8e5bb5", "#3f9b5c", "#c8465a", "#2a9d9d", "#b59a1e",
                "#b8509c", "#5865c9", "#6f9a2d",
            ]
            .map(String::from)
            .to_vec(),
            reference: ["#6b7280", "#d1242f", "#c4610f", "#2c8a3d", "#1f6fd1"]
                .map(String::from)
                .to_vec(),
            font_family: "Helvetica, Arial, sans-serif".into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: Some("#1e2126".into()),
            text: "#e6e8eb".into(),
            muted: "#9aa3ad".into(),
            grid: "#32373e".into(),
            axis: "#5d6671".into(),
            series: [
                "#5aa2e0", "#e8a05a", "#b48ad6", "#62bb7d", "#e07286", "#4fc0c0", "#d9bf4b",
                "#d77ab9", "#8a95e8", "#9bc65a",
            ]
            .map(String::from)
            .to_vec(),
            reference: ["#9aa3ad", "#f47067", "#e0883b", "#57ab5a", "#539bf5"]
                .map(String::from)
                .to_vec(),
            font_family: "Helvetica, Arial, sans-serif".into(),
        }
    }

    fn of(&self, role: Role) -> &str {
        match role {
            Role::Text => &self.text,
            Role::MutedText => &self.muted,
            Role::Grid => &self.grid,
            Role::Axis => &self.axis,
            Role::Series(index) => self
                .series
                .get(index % self.series.len().max(1))
                .map_or(self.text.as_str(), String::as_str),
            Role::Reference(color) => self
                .reference
                .get(color.index())
                .map_or(self.muted.as_str(), String::as_str),
        }
    }
}

fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn svg_item(item: &Item, style: &SvgStyle, out: &mut String) {
    use std::fmt::Write as _;
    // Writing to a String does not fail.
    match item {
        Item::Segment {
            from,
            to,
            role,
            width,
        } => {
            let _ = writeln!(
                out,
                r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width}"/>"#,
                from.0,
                from.1,
                to.0,
                to.1,
                style.of(*role)
            );
        }
        Item::DashedSegment {
            from,
            to,
            role,
            width,
        } => {
            let _ = writeln!(
                out,
                r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width}" stroke-dasharray="6 4"/>"#,
                from.0,
                from.1,
                to.0,
                to.1,
                style.of(*role)
            );
        }
        Item::Polyline {
            points,
            role,
            width,
        } => {
            let points: Vec<String> = points
                .iter()
                .map(|(x, y)| format!("{x:.2},{y:.2}"))
                .collect();
            let _ = writeln!(
                out,
                r#"<polyline points="{}" fill="none" stroke="{}" stroke-width="{width}" stroke-linejoin="round" stroke-linecap="round"/>"#,
                points.join(" "),
                style.of(*role)
            );
        }
        Item::Rect {
            rect,
            role,
            opacity,
        } => {
            let _ = writeln!(
                out,
                r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}" fill-opacity="{opacity}"/>"#,
                rect.left,
                rect.top,
                rect.width,
                rect.height,
                style.of(*role)
            );
        }
        Item::Circle {
            center,
            radius,
            role,
        } => {
            let _ = writeln!(
                out,
                r#"<circle cx="{:.2}" cy="{:.2}" r="{radius}" fill="{}"/>"#,
                center.0,
                center.1,
                style.of(*role)
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
            // The baseline is worked out here rather than asked of the renderer, which may not
            // know `dominant-baseline`.
            let baseline = match vertical {
                VerticalAnchor::Top => at.1 + size * 0.95,
                VerticalAnchor::Middle => at.1 + size * 0.35,
                VerticalAnchor::Bottom => at.1 - size * 0.3,
            };
            let anchor = match horizontal {
                Anchor::Start => "start",
                Anchor::Middle => "middle",
                Anchor::End => "end",
            };
            let weight = if *bold { r#" font-weight="bold""# } else { "" };
            let _ = writeln!(
                out,
                r#"<text x="{:.2}" y="{baseline:.2}" font-size="{size}" text-anchor="{anchor}" fill="{}"{weight}>{}</text>"#,
                at.0,
                style.of(*role),
                escape_xml(text)
            );
        }
    }
}

impl ChartScene {
    /// The chart as a stand-alone SVG picture, for copying or saving.
    pub fn to_svg(&self, style: &SvgStyle) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        // Writing to a String does not fail.
        let _ = writeln!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="{family}">"#,
            w = self.width,
            h = self.height,
            family = escape_xml(&style.font_family)
        );
        if let Some(background) = &style.background {
            let _ = writeln!(
                out,
                r#"<rect width="{}" height="{}" fill="{background}"/>"#,
                self.width, self.height
            );
        }
        let _ = writeln!(
            out,
            r#"<clipPath id="plot"><rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}"/></clipPath>"#,
            self.plot.left, self.plot.top, self.plot.width, self.plot.height
        );
        for item in &self.grid {
            svg_item(item, style, &mut out);
        }
        out.push_str("<g clip-path=\"url(#plot)\">\n");
        for item in &self.plot_items {
            svg_item(item, style, &mut out);
        }
        out.push_str("</g>\n");
        for item in &self.chrome {
            svg_item(item, style, &mut out);
        }
        out.push_str("</svg>\n");
        out
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
                Item::Segment { from, to, .. } | Item::DashedSegment { from, to, .. } => {
                    finite(*from) && finite(*to)
                }
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
                (line[0].0 - (scene.plot.left + DATA_INSET)).abs() < 0.01,
                "starts a little in from the left edge"
            );
            assert!(
                (line[9].0 - (scene.plot.right() - DATA_INSET)).abs() < 0.01,
                "ends a little in from the right edge"
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
    fn printed_values_leave_room_above_the_longest_bar() {
        let tallest = |scene: &ChartScene| {
            rects(&scene.plot_items)
                .iter()
                .map(|bar| bar.top)
                .fold(f32::INFINITY, f32::min)
        };
        let (_, plain) = scene_of(&states(), json!({"type": "Column"}));
        let (_, with_values) = scene_of(&states(), json!({"type": "Column", "showValues": true}));
        assert!(
            (tallest(&plain) - plain.plot.top).abs() < 1.0,
            "it fills the plot"
        );
        assert!(
            tallest(&with_values) > with_values.plot.top + 4.0,
            "it leaves a gap for the label"
        );
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

        // Either end can be set alone.
        let (_, top_only) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMax": 1000}),
        );
        let labels = texts(&top_only.chrome);
        assert!(labels.contains(&"1000".to_string()), "{labels:?}");
        assert!(
            labels.contains(&"0".to_string()),
            "the bottom stays automatic: {labels:?}"
        );
        let (_, bottom_only) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 40}),
        );
        let labels = texts(&bottom_only.chrome);
        assert!(labels.contains(&"40".to_string()), "{labels:?}");
        assert!(!labels.contains(&"0".to_string()), "{labels:?}");
        assert!(
            labels.contains(&"100".to_string()),
            "the top stays automatic: {labels:?}"
        );
        let (_, contradictory) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 500}),
        );
        assert!(
            texts(&contradictory.chrome).contains(&"100".to_string()),
            "a limit that leaves no room is ignored"
        );
    }

    #[test]
    fn a_stacked_to_full_chart_can_be_cut_to_a_range_of_percent() {
        let (_, scene) = scene_of(&states(), json!({"type": "ColumnStacked100", "yMax": 50}));
        let labels = texts(&scene.chrome);
        assert!(labels.contains(&"50%".to_string()), "{labels:?}");
        assert!(!labels.contains(&"100%".to_string()), "{labels:?}");
        // The segments are cut at the top: in each column the upper one reaches it.
        let bars = rects(&scene.plot_items);
        let tops: Vec<f32> = (0..3)
            .map(|category| bars[category].top.min(bars[category + 3].top))
            .collect();
        assert!(
            tops.iter().all(|top| (top - scene.plot.top).abs() < 1.0),
            "{tops:?}"
        );
    }

    #[test]
    fn the_marker_of_a_point_at_an_end_is_not_cut_by_the_edge_of_the_plot() {
        // Marker radius, and the ring drawn round it when hovering.
        const MARKER: f32 = 5.0;
        for kind in ["Line", "TimeLine"] {
            let (_, scene) = scene_of(&two_series_over_time(), json!({"type": kind}));
            let points: Vec<(f32, f32)> = scene
                .plot_items
                .iter()
                .filter_map(|item| match item {
                    Item::Polyline { points, .. } => Some(points.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert!(!points.is_empty());
            for (x, y) in points {
                assert!(
                    x - MARKER >= scene.plot.left && x + MARKER <= scene.plot.right(),
                    "{kind}: x {x}"
                );
                assert!(
                    y >= scene.plot.top && y <= scene.plot.bottom(),
                    "{kind}: y {y}"
                );
            }
        }
        // The same holds when the highest and the lowest values are the ends of the axis.
        let (_, limited) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 5, "yMax": 100}),
        );
        let hover = limited
            .hover(limited.plot.right() - 1.0, limited.plot.top + 10.0)
            .unwrap();
        assert!(
            hover
                .marks
                .iter()
                .all(|(x, _, _)| *x + MARKER <= limited.plot.right())
        );
    }

    /// The segments drawn in the colour of the axis, as `(horizontal, vertical)` counts.
    fn axis_segments(scene: &ChartScene) -> (usize, usize) {
        let mut counts = (0, 0);
        for item in &scene.chrome {
            if let Item::Segment {
                from,
                to,
                role: Role::Axis,
                ..
            } = item
            {
                if from.1 == to.1 && (to.0 - from.0).abs() > 20.0 {
                    counts.0 += 1;
                } else if from.0 == to.0 && (to.1 - from.1).abs() > 20.0 {
                    counts.1 += 1;
                }
            }
        }
        counts
    }

    fn grid_segments(scene: &ChartScene) -> (usize, usize) {
        let mut counts = (0, 0);
        for item in &scene.grid {
            if let Item::Segment { from, to, .. } = item {
                if from.1 == to.1 {
                    counts.0 += 1;
                } else {
                    counts.1 += 1;
                }
            }
        }
        counts
    }

    #[test]
    fn a_line_chart_has_one_axis_line_gridlines_across_the_values_and_ticks_under_the_labels() {
        let (_, scene) = scene_of(&two_series_over_time(), json!({"type": "TimeLine"}));
        assert_eq!(
            axis_segments(&scene),
            (1, 0),
            "only the axis along the bottom"
        );
        let (across, up) = grid_segments(&scene);
        assert!(
            across >= 3 && up == 0,
            "gridlines across the values only: {across} and {up}"
        );
        let ticks = scene
            .chrome
            .iter()
            .filter(|item| {
                matches!(item, Item::Segment { from, to, role: Role::Axis, .. }
                    if from.0 == to.0 && (to.1 - from.1).abs() < 10.0)
            })
            .count();
        assert!(ticks >= 3, "a tick under each label: {ticks}");
        // The lowest gridline is the axis line, not a line of its own above it.
        let lowest = scene
            .grid
            .iter()
            .filter_map(|item| match item {
                Item::Segment { from, to, .. } if from.1 == to.1 => Some(from.1),
                _ => None,
            })
            .fold(f32::MIN, f32::max);
        assert!(
            (lowest - scene.plot.bottom()).abs() < 1.0,
            "{lowest} against {}",
            scene.plot.bottom()
        );
    }

    #[test]
    fn a_horizontal_bar_chart_has_its_axis_line_at_the_left_and_gridlines_across_its_values() {
        let (_, scene) = scene_of(&states(), json!({"type": "Bar"}));
        assert_eq!(
            axis_segments(&scene),
            (0, 1),
            "the categories stand on the left"
        );
        let (across, up) = grid_segments(&scene);
        assert!(up >= 3 && across == 0, "{across} and {up}");
    }

    #[test]
    fn ticks_and_gridlines_can_be_set_for_either_axis() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "xShowGrid": true, "yShowGrid": false, "xShowTicks": false, "yShowTicks": true}),
        );
        let (across, up) = grid_segments(&scene);
        assert!(across == 0 && up >= 3, "{across} and {up}");
        let ticks_left = scene
            .chrome
            .iter()
            .filter(|item| {
                matches!(item, Item::Segment { from, to, role: Role::Axis, .. }
                    if from.1 == to.1 && (to.0 - from.0).abs() < 10.0)
            })
            .count();
        assert!(ticks_left >= 3, "ticks at the value labels: {ticks_left}");
    }

    /// The reference lines of a scene: `(from, to, dashed, role)`.
    fn reference_lines(scene: &ChartScene) -> Vec<((f32, f32), (f32, f32), bool, Role)> {
        scene
            .plot_items
            .iter()
            .filter_map(|item| match item {
                Item::DashedSegment {
                    from,
                    to,
                    role: role @ Role::Reference(_),
                    ..
                } => Some((*from, *to, true, *role)),
                Item::Segment {
                    from,
                    to,
                    role: role @ Role::Reference(_),
                    ..
                } => Some((*from, *to, false, *role)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_horizontal_line_is_dashed_at_its_value_and_a_vertical_one_is_at_its_time() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 0, "yMax": 100, "referenceLines": [
                {"orientation": "Horizontal", "value": 50},
                {"orientation": "Vertical", "value": "2007-01-05T00:00:00Z"}
            ]}),
        );
        let lines = reference_lines(&scene);
        assert_eq!(lines.len(), 2);
        let (from, to, dashed, role) = lines[0];
        assert!(dashed, "dashed unless it says solid");
        assert_eq!(role, Role::Reference(LineColor::Grey));
        assert_eq!(
            (from.0, to.0),
            (scene.plot.left, scene.plot.right()),
            "across the plot"
        );
        assert!(
            (from.1 - (scene.plot.top + scene.plot.height / 2.0)).abs() < 1.0,
            "half way up"
        );
        let (from, to, ..) = lines[1];
        assert_eq!(
            (from.1, to.1),
            (scene.plot.top, scene.plot.bottom()),
            "up the plot"
        );
        // Day 5 of ten days, inset from the ends like the data.
        let expected =
            scene.plot.left + DATA_INSET + (scene.plot.width - 2.0 * DATA_INSET) * 4.0 / 9.0;
        assert!(
            (from.0 - expected).abs() < 1.0,
            "{} against {expected}",
            from.0
        );
    }

    #[test]
    fn a_line_can_be_solid_and_coloured_and_have_a_label() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "referenceLines": [
                {"orientation": "Horizontal", "value": 50, "style": "Solid", "color": "Green", "label": "target"}
            ]}),
        );
        let lines = reference_lines(&scene);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].2, "solid");
        assert_eq!(lines[0].3, Role::Reference(LineColor::Green));
        let label = scene.chrome.iter().find_map(|item| match item {
            Item::Text { text, role, at, .. } if text == "target" => Some((*role, *at)),
            _ => None,
        });
        let (role, at) = label.expect("the label is drawn");
        assert_eq!(role, Role::Reference(LineColor::Green));
        assert!(
            scene.plot.contains(at.0 - 1.0, at.1),
            "inside the plot, at its right end"
        );
    }

    #[test]
    fn the_axes_grow_to_hold_a_line_unless_the_limits_say_otherwise() {
        let line = |value: i64| json!([{"orientation": "Horizontal", "value": value}]);
        let (_, grown) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "referenceLines": line(1000)}),
        );
        assert!(
            texts(&grown.chrome).contains(&"1k".to_string())
                || texts(&grown.chrome).contains(&"1000".to_string()),
            "{:?}",
            texts(&grown.chrome)
        );
        let inside = reference_lines(&grown)[0].0.1;
        assert!(
            inside >= grown.plot.top && inside <= grown.plot.bottom(),
            "in view"
        );

        let (_, limited) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yMin": 0, "yMax": 100, "referenceLines": line(1000)}),
        );
        let outside = reference_lines(&limited)[0].0.1;
        assert!(
            outside < limited.plot.top,
            "past the limit it is cut by the plot, not drawn inside"
        );
        assert!(
            !limited.chrome.iter().any(|item| matches!(
                item,
                Item::Text {
                    role: Role::Reference(_),
                    ..
                }
            )),
            "and has no label there"
        );

        // A vertical line past the last time moves the time axis out to it.
        let (_, later) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "referenceLines": [{"orientation": "Vertical", "value": "2007-03-01T00:00:00Z"}]}),
        );
        let x = reference_lines(&later)[0].0.0;
        assert!(
            x <= later.plot.right() && x > later.plot.left + later.plot.width * 0.9,
            "{x}"
        );
    }

    #[test]
    fn on_a_chart_of_categories_a_line_is_at_a_category_or_a_value() {
        let (_, column) = scene_of(
            &states(),
            json!({"type": "Column", "referenceLines": [
                {"orientation": "Vertical", "value": "CA"},
                {"orientation": "Horizontal", "value": 15},
                {"orientation": "Vertical", "value": "no such state"}
            ]}),
        );
        let lines = reference_lines(&column);
        assert_eq!(lines.len(), 2, "a category that is not there gets no line");
        let band = column.plot.width / 3.0;
        assert!(
            (lines[0].0.0 - (column.plot.left + band * 1.5)).abs() < 1.0,
            "the middle of CA's band"
        );

        // In horizontal bars the values run across and the categories down.
        let (_, bars) = scene_of(
            &states(),
            json!({"type": "Bar", "referenceLines": [
                {"orientation": "Vertical", "value": 20},
                {"orientation": "Horizontal", "value": "NY"}
            ]}),
        );
        let lines = reference_lines(&bars);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (lines[0].0.1, lines[0].1.1),
            (bars.plot.top, bars.plot.bottom()),
            "vertical at a value"
        );
        let band = bars.plot.height / 3.0;
        assert!(
            (lines[1].0.1 - (bars.plot.top + band * 2.5)).abs() < 1.0,
            "horizontal at NY's band"
        );
    }

    #[test]
    fn a_line_that_cannot_sit_on_a_log_axis_or_has_no_number_is_not_drawn() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "yAxis": "Log", "referenceLines": [
                {"orientation": "Horizontal", "value": 0},
                {"orientation": "Horizontal", "value": -5},
                {"orientation": "Horizontal", "value": "soon"},
                {"orientation": "Horizontal", "value": 20}
            ]}),
        );
        assert_eq!(reference_lines(&scene).len(), 1);
        assert!(all_coordinates_are_finite(&scene));
    }

    #[test]
    fn the_pointer_says_what_value_it_is_over_in_words_a_line_can_use() {
        let options = options(json!({"type": "TimeLine", "yMin": 0, "yMax": 100}));
        let (_, scene) = scene(&two_series_over_time(), &options, &ViewState::default());
        let point = scene
            .plot_items
            .iter()
            .find_map(|item| match item {
                Item::Polyline { points, .. } => Some(points[4]),
                _ => None,
            })
            .expect("a line");
        // Series a at day 5 is worth 50.
        let position = scene
            .pointer_position(point.0, point.1)
            .expect("inside the plot");
        assert_eq!(
            position.x.as_ref().map(|value| value.text.as_str()),
            Some("2007-01-05T00:00:00Z")
        );
        assert_eq!(
            position.y.as_ref().map(|value| value.text.as_str()),
            Some("50")
        );
        assert!(
            scene.pointer_position(1.0, 1.0).is_none(),
            "outside the plot"
        );

        // Those words, put in a line, draw it where the pointer was.
        let with_lines = {
            let mut options = options;
            options.reference_lines = Some(vec![
                ReferenceLine::new(
                    LineOrientation::Horizontal,
                    json!(position.y.unwrap().text.parse::<f64>().unwrap()),
                ),
                ReferenceLine::new(LineOrientation::Vertical, json!(position.x.unwrap().text)),
            ]);
            options
        };
        let (_, again) = scene_with_options(&two_series_over_time(), &with_lines);
        let lines = reference_lines(&again);
        assert!((lines[0].0.1 - point.1).abs() < 1.0);
        assert!((lines[1].0.0 - point.0).abs() < 1.0);

        // Categories are named, and a duration reads back.
        let (_, column) = scene_of(&states(), json!({"type": "Column"}));
        let second = column.plot.left + column.plot.width / 3.0 * 1.5;
        let position = column
            .pointer_position(second, column.plot.top + 5.0)
            .unwrap();
        assert_eq!(position.x.unwrap().text, "CA");
        let (_, bars) = scene_of(&states(), json!({"type": "Bar"}));
        let position = bars
            .pointer_position(
                bars.plot.left + 10.0,
                bars.plot.top + bars.plot.height / 3.0 * 2.5,
            )
            .unwrap();
        assert_eq!(
            position.y.unwrap().text,
            "NY",
            "the vertical axis names the category of a bar chart"
        );
        assert!(position.x.is_some());
    }

    fn scene_with_options(table: &Table, options: &ChartOptions) -> (ChartData, ChartScene) {
        scene(table, options, &ViewState::default())
    }

    #[test]
    fn a_dashed_line_is_written_dashed_in_the_svg_with_its_colour() {
        let (_, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "referenceLines": [
                {"orientation": "Horizontal", "value": 50, "color": "Red"},
                {"orientation": "Horizontal", "value": 60, "style": "Solid", "color": "Blue"}
            ]}),
        );
        let svg = scene.to_svg(&SvgStyle::light());
        assert_eq!(svg.matches("stroke-dasharray").count(), 1);
        assert!(svg.contains(&format!(
            r#"stroke="{}""#,
            SvgStyle::light().reference[LineColor::Red.index()]
        )));
        assert!(svg.contains(&format!(
            r#"stroke="{}""#,
            SvgStyle::light().reference[LineColor::Blue.index()]
        )));
    }

    #[test]
    fn bars_still_stand_on_the_axis_and_fill_their_bands() {
        let (_, scene) = scene_of(&states(), json!({"type": "Column"}));
        let bars = rects(&scene.plot_items);
        assert!((bars[0].bottom() - scene.plot.bottom()).abs() < 1.0);
        let first_band_left = scene.plot.left + scene.plot.width / 3.0 * 0.1;
        assert!(
            (bars[0].left - first_band_left).abs() < 1.0,
            "no inset along the bands"
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
    fn the_svg_holds_every_item_with_its_colours_and_escapes_text() {
        let (data, scene) = scene_of(
            &two_series_over_time(),
            json!({"type": "TimeLine", "title": "Rock & <Roll>"}),
        );
        let svg = scene.to_svg(&SvgStyle::light());
        assert!(svg.starts_with("<svg "));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<polyline").count(), 2);
        assert!(svg.contains("Rock &amp; &lt;Roll&gt;"));
        assert!(svg.contains(r##"fill="#ffffff""##), "a light background");
        assert!(svg.contains(&format!(
            r#"stroke="{}""#,
            SvgStyle::light().series[data.series[0].color_index]
        )));
        let dark = scene.to_svg(&SvgStyle::dark());
        assert!(dark.contains(r##"fill="#1e2126""##));
        let transparent = scene.to_svg(&SvgStyle {
            background: None,
            ..SvgStyle::light()
        });
        assert!(!transparent.contains(r##"fill="#ffffff""##));
    }

    #[test]
    fn the_svg_of_bars_and_gridlines_is_well_formed() {
        let (_, scene) = scene_of(
            &states(),
            json!({"type": "ColumnStacked", "showValues": true}),
        );
        let svg = scene.to_svg(&SvgStyle::dark());
        let mut reader = quick_xml::Reader::from_str(&svg);
        let mut elements = 0;
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(quick_xml::events::Event::Start(_) | quick_xml::events::Event::Empty(_)) => {
                    elements += 1
                }
                Ok(_) => {}
                Err(error) => panic!("not well-formed: {error}\n{svg}"),
            }
        }
        assert!(elements > 20);
        assert_eq!(
            svg.matches("<rect").count(),
            1 + 1 + 6 + 2,
            "background, clip, six bars, two legend swatches"
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
