//! The charts saved in a result file (`charts` in a `.ktt`), as the KustoTraceTools VS Code
//! extension writes them.
//!
//! Reading is forgiving and writing is faithful. An option this crate does not know, or a known
//! option whose value has an unexpected type, is kept as it was and written back unchanged, so
//! a chart of a type this fork does not draw survives a save.

use serde::ser::Error as _;
use serde::{Deserialize, Serialize, Serializer, de::DeserializeOwned};
use serde_json::{Map, Value};

/// A chart type, by the name the file spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChartType {
    Area,
    AreaStacked,
    AreaStacked100,
    Bar,
    BarStacked,
    BarStacked100,
    Card,
    Column,
    ColumnStacked,
    ColumnStacked100,
    Graph,
    Ladder,
    Line,
    Pie,
    Plotly,
    Sankey,
    Scatter,
    ThreeD,
    TimeLine,
    TimeLineAnomaly,
    TimePivot,
    TreeMap,
}

impl ChartType {
    pub const ALL: [ChartType; 22] = [
        ChartType::Area,
        ChartType::AreaStacked,
        ChartType::AreaStacked100,
        ChartType::Bar,
        ChartType::BarStacked,
        ChartType::BarStacked100,
        ChartType::Card,
        ChartType::Column,
        ChartType::ColumnStacked,
        ChartType::ColumnStacked100,
        ChartType::Graph,
        ChartType::Ladder,
        ChartType::Line,
        ChartType::Pie,
        ChartType::Plotly,
        ChartType::Sankey,
        ChartType::Scatter,
        ChartType::ThreeD,
        ChartType::TimeLine,
        ChartType::TimeLineAnomaly,
        ChartType::TimePivot,
        ChartType::TreeMap,
    ];

    /// The name stored in the file.
    pub fn name(self) -> &'static str {
        match self {
            ChartType::Area => "Area",
            ChartType::AreaStacked => "AreaStacked",
            ChartType::AreaStacked100 => "AreaStacked100",
            ChartType::Bar => "Bar",
            ChartType::BarStacked => "BarStacked",
            ChartType::BarStacked100 => "BarStacked100",
            ChartType::Card => "Card",
            ChartType::Column => "Column",
            ChartType::ColumnStacked => "ColumnStacked",
            ChartType::ColumnStacked100 => "ColumnStacked100",
            ChartType::Graph => "Graph",
            ChartType::Ladder => "Ladder",
            ChartType::Line => "Line",
            ChartType::Pie => "Pie",
            ChartType::Plotly => "Plotly",
            ChartType::Sankey => "Sankey",
            ChartType::Scatter => "Scatter",
            ChartType::ThreeD => "ThreeD",
            ChartType::TimeLine => "TimeLine",
            ChartType::TimeLineAnomaly => "TimeLineAnomaly",
            ChartType::TimePivot => "TimePivot",
            ChartType::TreeMap => "TreeMap",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|chart_type| chart_type.name() == name)
    }

    /// What the type is called in the `render` operator, and so in the chart picker.
    pub fn label(self) -> &'static str {
        match self {
            ChartType::Area => "Area",
            ChartType::AreaStacked => "Area, stacked",
            ChartType::AreaStacked100 => "Area, stacked 100%",
            ChartType::Bar => "Bar",
            ChartType::BarStacked => "Bar, stacked",
            ChartType::BarStacked100 => "Bar, stacked 100%",
            ChartType::Card => "Card",
            ChartType::Column => "Column",
            ChartType::ColumnStacked => "Column, stacked",
            ChartType::ColumnStacked100 => "Column, stacked 100%",
            ChartType::Graph => "Graph",
            ChartType::Ladder => "Time ladder",
            ChartType::Line => "Line",
            ChartType::Pie => "Pie",
            ChartType::Plotly => "Plotly",
            ChartType::Sankey => "Sankey",
            ChartType::Scatter => "Scatter",
            ChartType::ThreeD => "3D",
            ChartType::TimeLine => "Time line",
            ChartType::TimeLineAnomaly => "Time line with anomalies",
            ChartType::TimePivot => "Time pivot",
            ChartType::TreeMap => "Tree map",
        }
    }

    /// Whether this fork can draw the type.
    pub fn is_drawn(self) -> bool {
        matches!(
            self,
            ChartType::Line
                | ChartType::TimeLine
                | ChartType::Bar
                | ChartType::BarStacked
                | ChartType::BarStacked100
                | ChartType::Column
                | ChartType::ColumnStacked
                | ChartType::ColumnStacked100
        )
    }

    /// The types the chart picker offers.
    pub const DRAWN: [ChartType; 8] = [
        ChartType::Line,
        ChartType::TimeLine,
        ChartType::Bar,
        ChartType::BarStacked,
        ChartType::BarStacked100,
        ChartType::Column,
        ChartType::ColumnStacked,
        ChartType::ColumnStacked100,
    ];

    /// Whether a datetime column is the natural X axis of the type.
    pub fn prefers_time_axis(self) -> bool {
        matches!(
            self,
            ChartType::TimeLine
                | ChartType::TimeLineAnomaly
                | ChartType::Ladder
                | ChartType::TimePivot
        )
    }

    pub fn is_bar(self) -> bool {
        matches!(
            self,
            ChartType::Bar | ChartType::BarStacked | ChartType::BarStacked100
        )
    }

    pub fn is_column(self) -> bool {
        matches!(
            self,
            ChartType::Column | ChartType::ColumnStacked | ChartType::ColumnStacked100
        )
    }

    /// Bars or columns, grouped or stacked.
    pub fn is_bars(self) -> bool {
        self.is_bar() || self.is_column()
    }

    pub fn is_stacked(self) -> bool {
        matches!(
            self,
            ChartType::AreaStacked
                | ChartType::BarStacked
                | ChartType::ColumnStacked
                | ChartType::AreaStacked100
                | ChartType::BarStacked100
                | ChartType::ColumnStacked100
        )
    }

    /// Stacked so that every position adds up to 100%.
    pub fn is_stacked_to_full(self) -> bool {
        matches!(
            self,
            ChartType::AreaStacked100 | ChartType::BarStacked100 | ChartType::ColumnStacked100
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AxisScale {
    Linear,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    #[serde(alias = "Default")]
    Auto,
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegendPosition {
    Auto,
    Right,
    Bottom,
    #[serde(alias = "Hidden")]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMode {
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextSize {
    Auto,
    #[serde(rename = "Extra Small")]
    ExtraSmall,
    Small,
    Medium,
    Large,
    #[serde(rename = "Extra Large")]
    ExtraLarge,
}

impl TextSize {
    /// A multiple of the base text size; `Auto` follows the size of the chart.
    pub fn scale(self) -> Option<f32> {
        match self {
            TextSize::Auto => None,
            TextSize::ExtraSmall => Some(0.75),
            TextSize::Small => Some(0.875),
            TextSize::Medium => Some(1.0),
            TextSize::Large => Some(1.25),
            TextSize::ExtraLarge => Some(1.5),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AspectRatio {
    Fill,
    #[serde(rename = "16:9")]
    Wide,
    #[serde(rename = "3:2")]
    ThreeByTwo,
    #[serde(rename = "4:3")]
    FourByThree,
    #[serde(rename = "1:1")]
    Square,
    #[serde(rename = "3:4")]
    ThreeByFour,
    #[serde(rename = "2:3")]
    TwoByThree,
    #[serde(rename = "9:16")]
    Tall,
}

impl AspectRatio {
    /// Width divided by height; `Fill` has none because the chart takes the room it is given.
    pub fn ratio(self) -> Option<f32> {
        match self {
            AspectRatio::Fill => None,
            AspectRatio::Wide => Some(16. / 9.),
            AspectRatio::ThreeByTwo => Some(3. / 2.),
            AspectRatio::FourByThree => Some(4. / 3.),
            AspectRatio::Square => Some(1.),
            AspectRatio::ThreeByFour => Some(3. / 4.),
            AspectRatio::TwoByThree => Some(2. / 3.),
            AspectRatio::Tall => Some(9. / 16.),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum YLayout {
    SharedAxis,
    DualAxis,
    SeparatePanels,
    SeparateCharts,
}

/// How rows that have the same X in one series are combined into one point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aggregation {
    Sum,
    Count,
    Average,
    Min,
    Max,
}

impl Aggregation {
    pub const ALL: [Aggregation; 5] = [
        Aggregation::Sum,
        Aggregation::Average,
        Aggregation::Min,
        Aggregation::Max,
        Aggregation::Count,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Aggregation::Sum => "Sum",
            Aggregation::Count => "Count of rows",
            Aggregation::Average => "Average",
            Aggregation::Min => "Smallest",
            Aggregation::Max => "Largest",
        }
    }
}

/// The options of one chart. Anything not listed here is in `extra`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartOptions {
    /// The type as the file spells it, which may be one this crate has no name for. Empty when
    /// the file has none.
    pub chart_type: String,
    pub title: Option<String>,
    pub x_title: Option<String>,
    pub y_title: Option<String>,
    pub x_column: Option<String>,
    pub y_columns: Option<Vec<String>>,
    pub series_columns: Option<Vec<String>>,
    pub anomaly_columns: Option<Vec<String>>,
    pub x_axis: Option<AxisScale>,
    pub y_axis: Option<AxisScale>,
    /// Axis limits are numbers, or text for a datetime axis, so they are kept as they were read.
    pub x_min: Option<Value>,
    pub x_max: Option<Value>,
    pub y_min: Option<Value>,
    pub y_max: Option<Value>,
    pub y_layout: Option<YLayout>,
    pub accumulate: Option<bool>,
    pub show_values: Option<bool>,
    pub sort: Option<SortOrder>,
    pub legend_position: Option<LegendPosition>,
    pub mode: Option<ColorMode>,
    pub aspect_ratio: Option<AspectRatio>,
    pub text_size: Option<TextSize>,
    pub x_show_ticks: Option<bool>,
    pub y_show_ticks: Option<bool>,
    pub x_show_grid: Option<bool>,
    pub y_show_grid: Option<bool>,
    pub x_tick_angle: Option<f64>,
    pub y_tick_angle: Option<f64>,
    pub max_series: Option<u32>,
    pub show_markers: Option<bool>,
    /// Combines the rows of a series that have the same X. Without it each row is a point, and
    /// a line goes up and down where several rows share an X.
    pub aggregation: Option<Aggregation>,
    pub extra: Map<String, Value>,
}

impl ChartOptions {
    pub fn new(chart_type: ChartType) -> Self {
        Self {
            chart_type: chart_type.name().to_string(),
            ..Self::default()
        }
    }

    pub fn kind(&self) -> Option<ChartType> {
        ChartType::from_name(&self.chart_type)
    }

    pub fn set_kind(&mut self, chart_type: ChartType) {
        self.chart_type = chart_type.name().to_string();
    }

    /// Whether the gridlines at the ticks of the axis on the screen (the horizontal one, or the
    /// vertical one) are drawn: as the options say, else across the axis of the values only.
    pub fn grid_shown(&self, screen_horizontal: bool) -> bool {
        let explicit = if screen_horizontal {
            self.x_show_grid
        } else {
            self.y_show_grid
        };
        explicit.unwrap_or(screen_horizontal == self.kind().is_some_and(ChartType::is_bar))
    }

    /// Reads the options from a JSON object. Keys that are unknown, or whose value has the
    /// wrong type, stay in `extra`.
    pub fn from_map(mut map: Map<String, Value>) -> Self {
        let chart_type = match map.remove("type") {
            Some(Value::String(chart_type)) => chart_type,
            Some(other) => {
                map.insert("type".to_string(), other);
                String::new()
            }
            None => String::new(),
        };
        Self {
            chart_type,
            title: take(&mut map, "title"),
            x_title: take(&mut map, "xTitle"),
            y_title: take(&mut map, "yTitle"),
            x_column: take(&mut map, "xColumn"),
            y_columns: take(&mut map, "yColumns"),
            series_columns: take(&mut map, "seriesColumns"),
            anomaly_columns: take(&mut map, "anomalyColumns"),
            x_axis: take(&mut map, "xAxis"),
            y_axis: take(&mut map, "yAxis"),
            x_min: take_value(&mut map, "xMin"),
            x_max: take_value(&mut map, "xMax"),
            y_min: take_value(&mut map, "yMin"),
            y_max: take_value(&mut map, "yMax"),
            y_layout: take(&mut map, "yLayout"),
            accumulate: take(&mut map, "accumulate"),
            show_values: take(&mut map, "showValues"),
            sort: take(&mut map, "sort"),
            legend_position: take(&mut map, "legendPosition"),
            mode: take(&mut map, "mode"),
            aspect_ratio: take(&mut map, "aspectRatio"),
            text_size: take(&mut map, "textSize"),
            x_show_ticks: take(&mut map, "xShowTicks"),
            y_show_ticks: take(&mut map, "yShowTicks"),
            x_show_grid: take(&mut map, "xShowGrid"),
            y_show_grid: take(&mut map, "yShowGrid"),
            x_tick_angle: take(&mut map, "xTickAngle"),
            y_tick_angle: take(&mut map, "yTickAngle"),
            max_series: take(&mut map, "maxSeries"),
            show_markers: take(&mut map, "showMarkers"),
            aggregation: take(&mut map, "aggregation"),
            extra: map,
        }
    }

    /// The options a `render` operator asks for, from the JSON object the service sends in the
    /// `Visualization` row of `@ExtendedProperties`. `None` for a visualization this crate has
    /// no chart type for, such as `table`.
    pub fn from_render_properties(properties: &Value) -> Option<Self> {
        let visualization = render_property(properties, "Visualization")?.as_str()?;
        let stacking = render_property(properties, "Kind").and_then(Value::as_str);
        let stacked = |plain, stacked, stacked_to_full| match stacking {
            Some("stacked") => stacked,
            Some("stacked100") => stacked_to_full,
            _ => plain,
        };
        let chart_type = match visualization {
            "areachart" => stacked(
                ChartType::Area,
                ChartType::AreaStacked,
                ChartType::AreaStacked100,
            ),
            "stackedareachart" => ChartType::AreaStacked,
            "barchart" => stacked(
                ChartType::Bar,
                ChartType::BarStacked,
                ChartType::BarStacked100,
            ),
            "columnchart" => stacked(
                ChartType::Column,
                ChartType::ColumnStacked,
                ChartType::ColumnStacked100,
            ),
            "linechart" | "pivotchart" => ChartType::Line,
            "timechart" => ChartType::TimeLine,
            "anomalychart" => ChartType::TimeLineAnomaly,
            "ladderchart" => ChartType::Ladder,
            "timeline" | "timepivot" => ChartType::TimePivot,
            "scatterchart" => ChartType::Scatter,
            "piechart" => ChartType::Pie,
            "card" => ChartType::Card,
            "3Dchart" => ChartType::ThreeD,
            "treemap" => ChartType::TreeMap,
            "plotly" => ChartType::Plotly,
            "graph" => ChartType::Graph,
            "sankey" => ChartType::Sankey,
            _ => return None,
        };

        let mut options = Self::new(chart_type);
        options.title = render_text(properties, "Title");
        options.x_title = render_text(properties, "XTitle");
        options.y_title = render_text(properties, "YTitle");
        options.x_column = render_text(properties, "XColumn");
        options.y_columns = render_names(properties, "YColumns");
        options.series_columns = render_names(properties, "Series");
        options.anomaly_columns = render_names(properties, "AnomalyColumns");
        if render_text(properties, "XAxis").as_deref() == Some("log") {
            options.x_axis = Some(AxisScale::Log);
        }
        if render_text(properties, "YAxis").as_deref() == Some("log") {
            options.y_axis = Some(AxisScale::Log);
        }
        if render_text(properties, "Legend").as_deref() == Some("hidden") {
            options.legend_position = Some(LegendPosition::None);
        }
        options.y_layout = match render_text(properties, "YSplit").as_deref() {
            Some("axes") => Some(YLayout::DualAxis),
            Some("panels") => Some(YLayout::SeparatePanels),
            _ => None,
        };
        if render_property(properties, "Accumulate").and_then(Value::as_bool) == Some(true) {
            options.accumulate = Some(true);
        }
        options.x_min = render_limit(properties, "Xmin");
        options.x_max = render_limit(properties, "Xmax");
        options.y_min = render_limit(properties, "Ymin");
        options.y_max = render_limit(properties, "Ymax");
        Some(options)
    }

    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        if !self.chart_type.is_empty() {
            map.insert("type".to_string(), Value::String(self.chart_type.clone()));
        }
        put(&mut map, "mode", &self.mode);
        put(&mut map, "title", &self.title);
        put(&mut map, "xTitle", &self.x_title);
        put(&mut map, "yTitle", &self.y_title);
        put(&mut map, "xColumn", &self.x_column);
        put(&mut map, "yColumns", &self.y_columns);
        put(&mut map, "seriesColumns", &self.series_columns);
        put(&mut map, "anomalyColumns", &self.anomaly_columns);
        put(&mut map, "xAxis", &self.x_axis);
        put(&mut map, "yAxis", &self.y_axis);
        put(&mut map, "xMin", &self.x_min);
        put(&mut map, "xMax", &self.x_max);
        put(&mut map, "yMin", &self.y_min);
        put(&mut map, "yMax", &self.y_max);
        put(&mut map, "yLayout", &self.y_layout);
        put(&mut map, "accumulate", &self.accumulate);
        put(&mut map, "showValues", &self.show_values);
        put(&mut map, "sort", &self.sort);
        put(&mut map, "legendPosition", &self.legend_position);
        put(&mut map, "aspectRatio", &self.aspect_ratio);
        put(&mut map, "textSize", &self.text_size);
        put(&mut map, "xShowTicks", &self.x_show_ticks);
        put(&mut map, "yShowTicks", &self.y_show_ticks);
        put(&mut map, "xShowGrid", &self.x_show_grid);
        put(&mut map, "yShowGrid", &self.y_show_grid);
        put(&mut map, "xTickAngle", &self.x_tick_angle);
        put(&mut map, "yTickAngle", &self.y_tick_angle);
        put(&mut map, "maxSeries", &self.max_series);
        put(&mut map, "showMarkers", &self.show_markers);
        put(&mut map, "aggregation", &self.aggregation);
        for (key, value) in &self.extra {
            // A typed field wins over a stale unreadable value left under the same key.
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        map
    }
}

/// The service names its properties like `XTitle`; a response from another version may differ in
/// case, so a property is found without regard to it.
fn render_property<'a>(properties: &'a Value, name: &str) -> Option<&'a Value> {
    let object = properties.as_object()?;
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

fn render_text(properties: &Value, name: &str) -> Option<String> {
    render_property(properties, name)?
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn render_names(properties: &Value, name: &str) -> Option<Vec<String>> {
    let names: Vec<String> = render_property(properties, name)?
        .as_array()?
        .iter()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect();
    (!names.is_empty()).then_some(names)
}

/// An axis limit: a number, or the text of a datetime. The service writes "no limit" as `NaN`,
/// and a limit that is not finite cannot be drawn.
fn render_limit(properties: &Value, name: &str) -> Option<Value> {
    match render_property(properties, name)? {
        Value::Number(number) => Some(Value::Number(number.clone())),
        Value::String(text) => match text.parse::<f64>() {
            Ok(number) if number.is_finite() => {
                serde_json::Number::from_f64(number).map(Value::Number)
            }
            Ok(_) => None,
            Err(_) => (!text.is_empty()).then(|| Value::String(text.clone())),
        },
        _ => None,
    }
}

/// Removes `key` and reads it as `T`. A value that is not a `T` is put back so that it is
/// written out again.
fn take<T: DeserializeOwned>(map: &mut Map<String, Value>, key: &str) -> Option<T> {
    let value = map.remove(key)?;
    match serde_json::from_value(value.clone()) {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            map.insert(key.to_string(), value);
            None
        }
    }
}

/// Removes `key`, leaving a null in place as no value.
fn take_value(map: &mut Map<String, Value>, key: &str) -> Option<Value> {
    map.remove(key).filter(|value| !value.is_null())
}

fn put<T: Serialize>(map: &mut Map<String, Value>, key: &str, value: &Option<T>) {
    if let Some(value) = value
        && let Ok(value) = serde_json::to_value(value)
    {
        map.insert(key.to_string(), value);
    }
}

/// Which gridlines a chart draws when its own options say nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gridlines {
    /// Lines across the axis of the values only.
    Values,
    Both,
    None,
}

/// What a chart looks like when its own options say nothing, from the user's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartDefaults {
    pub legend_position: Option<LegendPosition>,
    pub show_values: Option<bool>,
    pub gridlines: Option<Gridlines>,
    pub text_size: Option<TextSize>,
    pub aspect_ratio: Option<AspectRatio>,
}

impl ChartOptions {
    /// The options with the defaults filled in where this chart has none of its own. What the
    /// chart saves is not changed: a default is not written to the file.
    pub fn with_defaults(&self, defaults: &ChartDefaults) -> ChartOptions {
        // The axis of the values is the horizontal one for bars and the vertical one otherwise.
        let values_are_across = self.kind().is_some_and(ChartType::is_bar);
        let default_grid = |screen_horizontal: bool| match defaults.gridlines {
            Some(Gridlines::Values) => Some(screen_horizontal == values_are_across),
            Some(Gridlines::Both) => Some(true),
            Some(Gridlines::None) => Some(false),
            None => None,
        };
        ChartOptions {
            legend_position: self.legend_position.or(defaults.legend_position),
            show_values: self.show_values.or(defaults.show_values),
            x_show_grid: self.x_show_grid.or(default_grid(true)),
            y_show_grid: self.y_show_grid.or(default_grid(false)),
            text_size: self.text_size.or(defaults.text_size),
            aspect_ratio: self.aspect_ratio.or(defaults.aspect_ratio),
            ..self.clone()
        }
    }
}

/// One chart of a result: which table it shows, what it is called, and how it looks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chart {
    pub name: Option<String>,
    /// The table the chart reads. A chart without one reads the first table.
    pub table_name: Option<String>,
    pub options: ChartOptions,
    /// Properties of the chart this crate does not know.
    pub extra: Map<String, Value>,
}

impl Chart {
    pub fn new(options: ChartOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    /// Reads a chart, or `None` when the value is not an object with an `options` object.
    pub fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut map) = value else {
            return None;
        };
        let Some(Value::Object(options)) = map.remove("options") else {
            return None;
        };
        Some(Self {
            name: take(&mut map, "name"),
            table_name: take(&mut map, "tableName"),
            options: ChartOptions::from_map(options),
            extra: map,
        })
    }

    /// Reads a `charts` property. Unless every entry is a chart the whole property is left
    /// alone, because a chart that could not be read cannot be written back.
    pub fn list_from_value(value: Value) -> Option<Vec<Chart>> {
        let Value::Array(entries) = value else {
            return None;
        };
        entries.into_iter().map(Chart::from_value).collect()
    }

    pub fn to_map(&self) -> Map<String, Value> {
        let mut map = Map::new();
        put(&mut map, "name", &self.name);
        put(&mut map, "tableName", &self.table_name);
        map.insert("options".to_string(), Value::Object(self.options.to_map()));
        for (key, value) in &self.extra {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
        map
    }

    /// The name shown on the chart's tab: its own, or `Chart n` for the chart at `index`.
    pub fn display_name(&self, index: usize) -> String {
        match self.name.as_deref().filter(|name| !name.trim().is_empty()) {
            Some(name) => name.to_string(),
            None => format!("Chart {}", index + 1),
        }
    }
}

impl Serialize for Chart {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Value::Object(self.to_map())
            .serialize(serializer)
            .map_err(S::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_the_options_a_vs_code_file_holds() {
        let chart = Chart::from_value(json!({
            "name": "Chart 2",
            "tableName": "PrimaryResult",
            "options": {"type": "Line", "mode": "Auto", "xAxis": "Linear", "yAxis": "Log",
                        "yColumns": ["a", "b"], "xMin": 3, "legendPosition": "Hidden",
                        "textSize": "Extra Large", "aspectRatio": "16:9"}
        }))
        .unwrap();
        assert_eq!(chart.name.as_deref(), Some("Chart 2"));
        assert_eq!(chart.table_name.as_deref(), Some("PrimaryResult"));
        assert_eq!(chart.options.kind(), Some(ChartType::Line));
        assert_eq!(chart.options.y_axis, Some(AxisScale::Log));
        assert_eq!(chart.options.y_columns, Some(vec!["a".into(), "b".into()]));
        assert_eq!(chart.options.x_min, Some(json!(3)));
        assert_eq!(chart.options.legend_position, Some(LegendPosition::None));
        assert_eq!(chart.options.text_size, Some(TextSize::ExtraLarge));
        assert_eq!(chart.options.aspect_ratio, Some(AspectRatio::Wide));
        assert!(chart.options.extra.is_empty());
    }

    #[test]
    fn options_this_crate_does_not_know_are_written_back() {
        let source = json!({
            "name": "g",
            "options": {"type": "Sankey", "nodesTable": "nodes", "markerShape": "Star",
                        "futureOption": {"nested": [1, 2]}},
            "futureProperty": true
        });
        let chart = Chart::from_value(source.clone()).unwrap();
        assert_eq!(chart.options.kind(), Some(ChartType::Sankey));
        assert_eq!(Value::Object(chart.to_map()), source);
    }

    #[test]
    fn a_value_of_the_wrong_type_is_kept_not_dropped() {
        let source =
            json!({"options": {"type": "Line", "xTickAngle": "steep", "xAxis": "Sideways"}});
        let chart = Chart::from_value(source.clone()).unwrap();
        assert_eq!(chart.options.x_tick_angle, None);
        assert_eq!(chart.options.x_axis, None);
        assert_eq!(Value::Object(chart.to_map()), source);
    }

    #[test]
    fn a_typed_value_replaces_an_unreadable_one_under_the_same_key() {
        let mut chart =
            Chart::from_value(json!({"options": {"type": "Line", "xAxis": "Sideways"}})).unwrap();
        chart.options.x_axis = Some(AxisScale::Log);
        assert_eq!(chart.to_map()["options"]["xAxis"], json!("Log"));
    }

    #[test]
    fn unset_options_are_not_written() {
        let chart = Chart::new(ChartOptions::new(ChartType::Column));
        assert_eq!(
            Value::Object(chart.to_map()),
            json!({"options": {"type": "Column"}})
        );
    }

    #[test]
    fn a_list_with_an_unreadable_entry_is_not_read_at_all() {
        let list = json!([{"options": {"type": "Line"}}, {"name": "no options"}]);
        assert_eq!(Chart::list_from_value(list), None);
        assert_eq!(Chart::list_from_value(json!({"options": {}})), None);
    }

    #[test]
    fn a_default_fills_what_the_chart_leaves_open_and_never_overrides_it() {
        let defaults = ChartDefaults {
            legend_position: Some(LegendPosition::Bottom),
            show_values: Some(true),
            gridlines: Some(Gridlines::None),
            text_size: Some(TextSize::Large),
            ..ChartDefaults::default()
        };
        let mut options = ChartOptions::new(ChartType::Column);
        options.legend_position = Some(LegendPosition::None);
        options.y_show_grid = Some(true);
        let effective = options.with_defaults(&defaults);
        assert_eq!(
            effective.legend_position,
            Some(LegendPosition::None),
            "the chart's own wins"
        );
        assert_eq!(effective.show_values, Some(true));
        assert_eq!(effective.x_show_grid, Some(false));
        assert_eq!(effective.y_show_grid, Some(true), "the chart's own wins");
        assert_eq!(effective.text_size, Some(TextSize::Large));
        assert_eq!(
            options.show_values, None,
            "the original is untouched, so nothing is saved"
        );
        assert_eq!(effective.chart_type, "Column");
    }

    #[test]
    fn the_gridlines_of_a_chart_follow_its_axis_of_values_unless_it_says_otherwise() {
        let values = ChartDefaults {
            gridlines: Some(Gridlines::Values),
            ..ChartDefaults::default()
        };
        let column = ChartOptions::new(ChartType::Column).with_defaults(&values);
        assert_eq!(
            (column.x_show_grid, column.y_show_grid),
            (Some(false), Some(true))
        );
        let bar = ChartOptions::new(ChartType::Bar).with_defaults(&values);
        assert_eq!(
            (bar.x_show_grid, bar.y_show_grid),
            (Some(true), Some(false))
        );
        let both = ChartOptions::new(ChartType::Line).with_defaults(&ChartDefaults {
            gridlines: Some(Gridlines::Both),
            ..ChartDefaults::default()
        });
        assert_eq!(
            (both.x_show_grid, both.y_show_grid),
            (Some(true), Some(true))
        );
        // With no setting the chart's own options, or what its type draws, decide.
        let line = ChartOptions::new(ChartType::TimeLine);
        assert!(!line.grid_shown(true) && line.grid_shown(false));
        let bar = ChartOptions::new(ChartType::Bar);
        assert!(bar.grid_shown(true) && !bar.grid_shown(false));
        let mut explicit = ChartOptions::new(ChartType::TimeLine);
        explicit.x_show_grid = Some(true);
        explicit.y_show_grid = Some(false);
        assert!(explicit.grid_shown(true) && !explicit.grid_shown(false));
    }

    #[test]
    fn the_tab_name_falls_back_to_the_position() {
        let mut chart = Chart::new(ChartOptions::new(ChartType::Line));
        assert_eq!(chart.display_name(1), "Chart 2");
        chart.name = Some("  ".into());
        assert_eq!(chart.display_name(0), "Chart 1");
        chart.name = Some("Latency".into());
        assert_eq!(chart.display_name(0), "Latency");
    }

    fn render(properties: Value) -> Option<ChartOptions> {
        ChartOptions::from_render_properties(&properties)
    }

    #[test]
    fn render_timechart_with_a_title_as_the_service_sends_it() {
        let options = render(json!({
            "Visualization": "timechart", "Title": "Storm events", "XColumn": null,
            "Series": null, "YColumns": null, "AnomalyColumns": null, "XTitle": null,
            "YTitle": "Events", "XAxis": null, "YAxis": null, "Legend": null, "YSplit": null,
            "Accumulate": false, "IsQuerySorted": true, "Kind": null, "Ymin": "NaN",
            "Ymax": "NaN", "Xmin": null, "Xmax": null
        }))
        .unwrap();
        let mut expected = ChartOptions::new(ChartType::TimeLine);
        expected.title = Some("Storm events".into());
        expected.y_title = Some("Events".into());
        assert_eq!(options, expected);
    }

    #[test]
    fn render_kinds_choose_the_stacked_types() {
        let kind = |visualization: &str, kind: Value| {
            render(json!({"Visualization": visualization, "Kind": kind}))
                .and_then(|options| options.kind())
        };
        assert_eq!(kind("barchart", json!(null)), Some(ChartType::Bar));
        assert_eq!(kind("barchart", json!("default")), Some(ChartType::Bar));
        assert_eq!(
            kind("barchart", json!("stacked")),
            Some(ChartType::BarStacked)
        );
        assert_eq!(
            kind("columnchart", json!("stacked100")),
            Some(ChartType::ColumnStacked100)
        );
        assert_eq!(
            kind("columnchart", json!("unstacked")),
            Some(ChartType::Column)
        );
        assert_eq!(
            kind("areachart", json!("stacked")),
            Some(ChartType::AreaStacked)
        );
        assert_eq!(
            kind("stackedareachart", json!(null)),
            Some(ChartType::AreaStacked)
        );
        assert_eq!(kind("linechart", json!(null)), Some(ChartType::Line));
        assert_eq!(kind("pivotchart", json!(null)), Some(ChartType::Line));
        assert_eq!(kind("timepivot", json!(null)), Some(ChartType::TimePivot));
        assert_eq!(kind("table", json!(null)), None);
        assert_eq!(kind("", json!(null)), None);
    }

    #[test]
    fn render_properties_map_to_options() {
        let options = render(json!({
            "Visualization": "linechart", "XColumn": "T", "Series": ["Region", "Tier"],
            "YColumns": ["A", "B"], "XTitle": "when", "YAxis": "log", "XAxis": "linear",
            "Legend": "hidden", "YSplit": "panels", "Accumulate": true,
            "Ymin": 0, "Ymax": "100.5", "Xmin": "2026-01-01T00:00:00Z", "Xmax": "NaN"
        }))
        .unwrap();
        assert_eq!(options.x_column.as_deref(), Some("T"));
        assert_eq!(
            options.series_columns,
            Some(vec!["Region".into(), "Tier".into()])
        );
        assert_eq!(options.y_columns, Some(vec!["A".into(), "B".into()]));
        assert_eq!(options.x_title.as_deref(), Some("when"));
        assert_eq!(options.y_axis, Some(AxisScale::Log));
        assert_eq!(options.x_axis, None);
        assert_eq!(options.legend_position, Some(LegendPosition::None));
        assert_eq!(options.y_layout, Some(YLayout::SeparatePanels));
        assert_eq!(options.accumulate, Some(true));
        assert_eq!(options.y_min, Some(json!(0)));
        assert_eq!(options.y_max, Some(json!(100.5)));
        assert_eq!(options.x_min, Some(json!("2026-01-01T00:00:00Z")));
        assert_eq!(options.x_max, None);
    }

    #[test]
    fn render_property_names_are_found_without_regard_to_case() {
        let options = render(json!({"visualization": "barchart", "xtitle": "x"})).unwrap();
        assert_eq!(options.x_title.as_deref(), Some("x"));
    }

    #[test]
    fn every_type_name_reads_back() {
        for chart_type in ChartType::ALL {
            assert_eq!(ChartType::from_name(chart_type.name()), Some(chart_type));
        }
        assert_eq!(ChartType::from_name("Donut"), None);
    }
}
