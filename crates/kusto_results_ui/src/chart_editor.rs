//! The pane that edits a chart: what it draws, how it is titled and how its axes look (CED).
//!
//! The editor changes a copy of the chart and reports each change; the view that hosts it
//! redraws, and the viewer saves. Only the options that can be edited here are shown. The rest
//! are kept by the chart as they were.

use editor::{Editor, EditorEvent};
use gpui::{
    Anchor, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, WeakEntity, Window, div, px,
};
use gpui_util::ResultExt as _;
use kusto_results::chart::{AspectRatio, AxisScale, LegendPosition, SortOrder, TextSize};
use kusto_results::{Chart, ChartType};
use serde_json::{Number, Value};
use ui::{Button, Checkbox, ContextMenu, DropdownMenu, PopoverMenuHandle, ToggleState, prelude::*};

pub(crate) enum ChartEditorEvent {
    Changed(Chart),
    Remove,
}

/// A column of the table the chart reads.
#[derive(Clone)]
pub(crate) struct ColumnInfo {
    pub name: SharedString,
    pub numeric: bool,
}

/// The columns a chart reads once what the options leave open is decided, so the checkboxes can
/// show what is drawn when the options say nothing.
#[derive(Clone, Default)]
pub(crate) struct Effective {
    pub x: Option<String>,
    pub values: Vec<String>,
    pub series: Vec<String>,
}

struct Dropdown {
    menu: Entity<ContextMenu>,
    handle: PopoverMenuHandle<ContextMenu>,
}

struct Input {
    editor: Entity<Editor>,
    _subscription: Subscription,
}

pub(crate) struct ChartEditor {
    focus_handle: FocusHandle,
    chart: Chart,
    columns: Vec<ColumnInfo>,
    effective: Effective,
    chart_type: Dropdown,
    table: Option<Dropdown>,
    x_column: Dropdown,
    x_scale: Dropdown,
    y_scale: Dropdown,
    legend: Dropdown,
    sort: Dropdown,
    aspect_ratio: Dropdown,
    text_size: Dropdown,
    name: Input,
    title: Input,
    x_title: Input,
    y_title: Input,
    x_min: Input,
    x_max: Input,
    y_min: Input,
    y_max: Input,
    max_series: Input,
}

impl EventEmitter<ChartEditorEvent> for ChartEditor {}

impl Focusable for ChartEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn text_of(value: &Option<String>) -> String {
    value.clone().unwrap_or_default()
}

/// An axis limit as typed: a number when it is one, else the text, which a time axis reads.
fn limit_from_text(text: &str) -> Option<Value> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.parse::<f64>() {
        Ok(number) => Number::from_f64(number).map(Value::Number),
        Err(_) => Some(Value::String(text.to_string())),
    }
}

fn limit_text(limit: &Option<Value>) -> String {
    match limit {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

fn toggle(names: &mut Vec<String>, name: &str) {
    match names.iter().position(|existing| existing == name) {
        Some(position) => {
            names.remove(position);
        }
        None => names.push(name.to_string()),
    }
}

fn state(on: bool) -> ToggleState {
    if on {
        ToggleState::Selected
    } else {
        ToggleState::Unselected
    }
}

impl ChartEditor {
    pub(crate) fn new(
        chart: Chart,
        tables: Vec<SharedString>,
        columns: Vec<ColumnInfo>,
        effective: Effective,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = chart.options.clone();
        let chart_type = Self::dropdown(
            ChartType::DRAWN
                .iter()
                .map(|kind| (SharedString::from(kind.label()), *kind))
                .collect(),
            |chart, kind| chart.options.set_kind(kind),
            window,
            cx,
        );
        let table = (tables.len() > 1).then(|| {
            Self::dropdown(
                tables
                    .iter()
                    .map(|name| (name.clone(), name.clone()))
                    .collect(),
                |chart, name: SharedString| {
                    chart.table_name = Some(name.to_string());
                    // The columns belong to the table, so what was chosen among them no longer
                    // means anything.
                    chart.options.x_column = None;
                    chart.options.y_columns = None;
                    chart.options.series_columns = None;
                },
                window,
                cx,
            )
        });
        let x_column = Self::dropdown(
            std::iter::once((SharedString::from("Auto"), None))
                .chain(
                    columns
                        .iter()
                        .map(|column| (column.name.clone(), Some(column.name.to_string()))),
                )
                .collect(),
            |chart, name: Option<String>| chart.options.x_column = name,
            window,
            cx,
        );
        let scale_choices = vec![
            (SharedString::from("Linear"), None),
            (SharedString::from("Log"), Some(AxisScale::Log)),
        ];
        let x_scale = Self::dropdown(
            scale_choices.clone(),
            |chart, scale: Option<AxisScale>| chart.options.x_axis = scale,
            window,
            cx,
        );
        let y_scale = Self::dropdown(
            scale_choices,
            |chart, scale: Option<AxisScale>| chart.options.y_axis = scale,
            window,
            cx,
        );
        let legend = Self::dropdown(
            vec![
                ("Auto".into(), None),
                ("Right".into(), Some(LegendPosition::Right)),
                ("Bottom".into(), Some(LegendPosition::Bottom)),
                ("None".into(), Some(LegendPosition::None)),
            ],
            |chart, position: Option<LegendPosition>| chart.options.legend_position = position,
            window,
            cx,
        );
        let sort = Self::dropdown(
            vec![
                ("Auto".into(), None),
                ("Ascending".into(), Some(SortOrder::Ascending)),
                ("Descending".into(), Some(SortOrder::Descending)),
            ],
            |chart, order: Option<SortOrder>| chart.options.sort = order,
            window,
            cx,
        );
        let aspect_ratio = Self::dropdown(
            vec![
                ("Fill".into(), None),
                ("16:9".into(), Some(AspectRatio::Wide)),
                ("3:2".into(), Some(AspectRatio::ThreeByTwo)),
                ("4:3".into(), Some(AspectRatio::FourByThree)),
                ("1:1".into(), Some(AspectRatio::Square)),
                ("3:4".into(), Some(AspectRatio::ThreeByFour)),
                ("2:3".into(), Some(AspectRatio::TwoByThree)),
                ("9:16".into(), Some(AspectRatio::Tall)),
            ],
            |chart, ratio: Option<AspectRatio>| chart.options.aspect_ratio = ratio,
            window,
            cx,
        );
        let text_size = Self::dropdown(
            vec![
                ("Auto".into(), None),
                ("Extra small".into(), Some(TextSize::ExtraSmall)),
                ("Small".into(), Some(TextSize::Small)),
                ("Medium".into(), Some(TextSize::Medium)),
                ("Large".into(), Some(TextSize::Large)),
                ("Extra large".into(), Some(TextSize::ExtraLarge)),
            ],
            |chart, size: Option<TextSize>| chart.options.text_size = size,
            window,
            cx,
        );

        let name = Self::input(
            &text_of(&chart.name),
            "Chart name",
            |chart, text| chart.name = non_empty(text),
            window,
            cx,
        );
        let title = Self::input(
            &text_of(&options.title),
            "None",
            |chart, text| chart.options.title = non_empty(text),
            window,
            cx,
        );
        let x_title = Self::input(
            &text_of(&options.x_title),
            "None",
            |chart, text| chart.options.x_title = non_empty(text),
            window,
            cx,
        );
        let y_title = Self::input(
            &text_of(&options.y_title),
            "None",
            |chart, text| chart.options.y_title = non_empty(text),
            window,
            cx,
        );
        let x_min = Self::input(
            &limit_text(&options.x_min),
            "Auto",
            |chart, text| chart.options.x_min = limit_from_text(text),
            window,
            cx,
        );
        let x_max = Self::input(
            &limit_text(&options.x_max),
            "Auto",
            |chart, text| chart.options.x_max = limit_from_text(text),
            window,
            cx,
        );
        let y_min = Self::input(
            &limit_text(&options.y_min),
            "Auto",
            |chart, text| chart.options.y_min = limit_from_text(text),
            window,
            cx,
        );
        let y_max = Self::input(
            &limit_text(&options.y_max),
            "Auto",
            |chart, text| chart.options.y_max = limit_from_text(text),
            window,
            cx,
        );
        let max_series = Self::input(
            &options
                .max_series
                .map(|count| count.to_string())
                .unwrap_or_default(),
            "All",
            |chart, text| {
                chart.options.max_series = text.trim().parse().ok().filter(|count| *count > 0)
            },
            window,
            cx,
        );

        Self {
            focus_handle: cx.focus_handle(),
            chart,
            columns,
            effective,
            chart_type,
            table,
            x_column,
            x_scale,
            y_scale,
            legend,
            sort,
            aspect_ratio,
            text_size,
            name,
            title,
            x_title,
            y_title,
            x_min,
            x_max,
            y_min,
            y_max,
            max_series,
        }
    }

    /// A list of choices; picking one applies its value to the chart.
    fn dropdown<T: Clone + 'static>(
        choices: Vec<(SharedString, T)>,
        apply: impl Fn(&mut Chart, T) + Clone + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Dropdown {
        let editor = cx.weak_entity();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for (label, value) in choices {
                let editor = editor.clone();
                let apply = apply.clone();
                menu = menu.entry(label, None, move |_, cx| {
                    editor
                        .update(cx, |this, cx| {
                            apply(&mut this.chart, value.clone());
                            this.changed(cx);
                        })
                        .log_err();
                });
            }
            menu
        });
        Dropdown {
            menu,
            handle: PopoverMenuHandle::default(),
        }
    }

    /// A line of text; each edit applies what was typed to the chart.
    fn input(
        initial: &str,
        placeholder: &'static str,
        apply: fn(&mut Chart, &str),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Input {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(initial, window, cx);
            editor.set_placeholder_text(placeholder, window, cx);
            editor
        });
        let _subscription = cx.subscribe(&editor, move |this, editor, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                let text = editor.read(cx).text(cx);
                apply(&mut this.chart, &text);
                this.changed(cx);
            }
        });
        Input {
            editor,
            _subscription,
        }
    }

    #[cfg(test)]
    pub(crate) fn edit(&mut self, change: impl FnOnce(&mut Chart), cx: &mut Context<Self>) {
        change(&mut self.chart);
        self.changed(cx);
    }

    #[cfg(test)]
    pub(crate) fn title_editor(&self) -> Entity<Editor> {
        self.title.editor.clone()
    }

    /// Tells the editor which columns the chart reads now, which changes when the data does.
    pub(crate) fn set_effective(&mut self, effective: Effective, cx: &mut Context<Self>) {
        self.effective = effective;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ChartEditorEvent::Changed(self.chart.clone()));
        cx.notify();
    }

    fn toggle_value_column(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut names = match &self.chart.options.y_columns {
            Some(names) if !names.is_empty() => names.clone(),
            _ => self.effective.values.clone(),
        };
        toggle(&mut names, name);
        self.chart.options.y_columns = (!names.is_empty()).then_some(names);
        self.changed(cx);
    }

    fn toggle_series_column(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut names = match &self.chart.options.series_columns {
            Some(names) if !names.is_empty() => names.clone(),
            _ => self.effective.series.clone(),
        };
        toggle(&mut names, name);
        self.chart.options.series_columns = (!names.is_empty()).then_some(names);
        self.changed(cx);
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        h_flex()
            .justify_between()
            .items_center()
            .gap_2()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(control)
    }

    fn section(title: &'static str) -> impl IntoElement {
        Label::new(title)
            .size(LabelSize::Small)
            .weight(gpui::FontWeight::BOLD)
    }

    fn dropdown_button(
        id: &'static str,
        label: impl Into<SharedString>,
        dropdown: &Dropdown,
    ) -> DropdownMenu {
        DropdownMenu::new(id, label, dropdown.menu.clone())
            .handle(dropdown.handle.clone())
            .attach(Anchor::BottomLeft)
    }

    fn input_box(input: &Input, border: gpui::Hsla) -> Div {
        div()
            .w(px(150.))
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(border)
            .rounded_sm()
            .child(input.editor.clone())
    }

    fn checkbox(
        id: &'static str,
        label: &'static str,
        on: bool,
        editor: &WeakEntity<Self>,
        apply: impl Fn(&mut Chart, bool) + 'static,
    ) -> Checkbox {
        let editor = editor.clone();
        Checkbox::new(id, state(on))
            .label(label)
            .label_size(LabelSize::Small)
            .on_click(move |new_state, _, cx| {
                let on = matches!(new_state, ToggleState::Selected);
                editor
                    .update(cx, |this, cx| {
                        apply(&mut this.chart, on);
                        this.changed(cx);
                    })
                    .log_err();
            })
    }

    fn column_checkbox(
        id: (&'static str, usize),
        name: &SharedString,
        on: bool,
        editor: &WeakEntity<Self>,
        toggle: fn(&mut Self, &str, &mut Context<Self>),
    ) -> Checkbox {
        let editor = editor.clone();
        let column = name.clone();
        Checkbox::new(id, state(on))
            .label(name.clone())
            .label_size(LabelSize::Small)
            .on_click(move |_, _, cx| {
                editor
                    .update(cx, |this, cx| toggle(this, &column, cx))
                    .log_err();
            })
    }
}

fn scale_label(scale: Option<AxisScale>) -> &'static str {
    match scale {
        Some(AxisScale::Log) => "Log",
        _ => "Linear",
    }
}

fn legend_label(position: Option<LegendPosition>) -> &'static str {
    match position {
        Some(LegendPosition::Right) => "Right",
        Some(LegendPosition::Bottom) => "Bottom",
        Some(LegendPosition::None) => "None",
        _ => "Auto",
    }
}

fn sort_label(order: Option<SortOrder>) -> &'static str {
    match order {
        Some(SortOrder::Ascending) => "Ascending",
        Some(SortOrder::Descending) => "Descending",
        _ => "Auto",
    }
}

fn aspect_label(ratio: Option<AspectRatio>) -> &'static str {
    match ratio {
        None | Some(AspectRatio::Fill) => "Fill",
        Some(AspectRatio::Wide) => "16:9",
        Some(AspectRatio::ThreeByTwo) => "3:2",
        Some(AspectRatio::FourByThree) => "4:3",
        Some(AspectRatio::Square) => "1:1",
        Some(AspectRatio::ThreeByFour) => "3:4",
        Some(AspectRatio::TwoByThree) => "2:3",
        Some(AspectRatio::Tall) => "9:16",
    }
}

fn text_size_label(size: Option<TextSize>) -> &'static str {
    match size {
        None | Some(TextSize::Auto) => "Auto",
        Some(TextSize::ExtraSmall) => "Extra small",
        Some(TextSize::Small) => "Small",
        Some(TextSize::Medium) => "Medium",
        Some(TextSize::Large) => "Large",
        Some(TextSize::ExtraLarge) => "Extra large",
    }
}

impl Render for ChartEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options = self.chart.options.clone();
        let border = cx.theme().colors().border;
        let editor = cx.weak_entity();
        let type_label = options.kind().map_or_else(
            || options.chart_type.clone(),
            |kind| kind.label().to_string(),
        );
        let x_label = match (&options.x_column, &self.effective.x) {
            (Some(name), _) => name.clone(),
            (None, Some(effective)) => format!("Auto ({effective})"),
            (None, None) => "Auto".to_string(),
        };
        let value_columns: Vec<_> =
            self.columns
                .iter()
                .enumerate()
                .filter(|(_, column)| {
                    column.numeric
                        || options.y_columns.as_ref().is_some_and(|names| {
                            names.iter().any(|name| name == column.name.as_ref())
                        })
                })
                .map(|(index, column)| (index, column.name.clone()))
                .collect();
        let value_checkboxes: Vec<_> = value_columns
            .iter()
            .map(|(index, name)| {
                let on = match &options.y_columns {
                    Some(names) if !names.is_empty() => names.iter().any(|n| n == name.as_ref()),
                    _ => self.effective.values.iter().any(|n| n == name.as_ref()),
                };
                Self::column_checkbox(
                    ("chart-value-column", *index),
                    name,
                    on,
                    &editor,
                    Self::toggle_value_column,
                )
            })
            .collect();
        let series_checkboxes: Vec<_> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let on = match &options.series_columns {
                    Some(names) if !names.is_empty() => {
                        names.iter().any(|n| n == column.name.as_ref())
                    }
                    _ => self
                        .effective
                        .series
                        .iter()
                        .any(|n| n == column.name.as_ref()),
                };
                Self::column_checkbox(
                    ("chart-series-column", index),
                    &column.name,
                    on,
                    &editor,
                    Self::toggle_series_column,
                )
            })
            .collect();
        let table_label = self.chart.table_name.clone().unwrap_or_default();

        v_flex()
            .id("chart-editor")
            .debug_selector(|| "chart-editor".to_string())
            .track_focus(&self.focus_handle)
            .w(px(300.))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .gap_2()
            .p_2()
            .border_l_1()
            .border_color(border)
            .child(Self::section("Chart"))
            .child(Self::row("Name", Self::input_box(&self.name, border)))
            .child(Self::row(
                "Type",
                Self::dropdown_button("chart-type", type_label, &self.chart_type),
            ))
            .children(self.table.as_ref().map(|table| {
                Self::row(
                    "Table",
                    Self::dropdown_button("chart-table", table_label, table),
                )
            }))
            .child(Self::section("Data"))
            .child(Self::row(
                "X",
                Self::dropdown_button("chart-x-column", x_label, &self.x_column),
            ))
            .child(
                Label::new("Values")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .children(value_checkboxes)
            .child(
                Label::new("Split into series by")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .children(series_checkboxes)
            .child(Self::row(
                "Most series",
                Self::input_box(&self.max_series, border),
            ))
            .child(Self::checkbox(
                "chart-accumulate",
                "Running total",
                options.accumulate == Some(true),
                &editor,
                |chart, on| chart.options.accumulate = on.then_some(true),
            ))
            .child(Self::row(
                "Sort",
                Self::dropdown_button("chart-sort", sort_label(options.sort), &self.sort),
            ))
            .child(Self::section("Titles"))
            .child(Self::row("Title", Self::input_box(&self.title, border)))
            .child(Self::row(
                "Horizontal axis",
                Self::input_box(&self.x_title, border),
            ))
            .child(Self::row(
                "Vertical axis",
                Self::input_box(&self.y_title, border),
            ))
            .child(Self::section("Axes"))
            .child(Self::row(
                "Horizontal scale",
                Self::dropdown_button("chart-x-scale", scale_label(options.x_axis), &self.x_scale),
            ))
            .child(Self::row(
                "Horizontal from",
                Self::input_box(&self.x_min, border),
            ))
            .child(Self::row(
                "Horizontal to",
                Self::input_box(&self.x_max, border),
            ))
            .child(Self::row(
                "Vertical scale",
                Self::dropdown_button("chart-y-scale", scale_label(options.y_axis), &self.y_scale),
            ))
            .child(Self::row(
                "Vertical from",
                Self::input_box(&self.y_min, border),
            ))
            .child(Self::row(
                "Vertical to",
                Self::input_box(&self.y_max, border),
            ))
            .child(Self::checkbox(
                "chart-x-grid",
                "Vertical gridlines",
                options.x_show_grid != Some(false),
                &editor,
                |chart, on| chart.options.x_show_grid = (!on).then_some(false),
            ))
            .child(Self::checkbox(
                "chart-y-grid",
                "Horizontal gridlines",
                options.y_show_grid != Some(false),
                &editor,
                |chart, on| chart.options.y_show_grid = (!on).then_some(false),
            ))
            .child(Self::section("Look"))
            .child(Self::row(
                "Legend",
                Self::dropdown_button(
                    "chart-legend",
                    legend_label(options.legend_position),
                    &self.legend,
                ),
            ))
            .child(Self::checkbox(
                "chart-show-values",
                "Show values on bars",
                options.show_values == Some(true),
                &editor,
                |chart, on| chart.options.show_values = on.then_some(true),
            ))
            .child(Self::checkbox(
                "chart-show-markers",
                "Mark the points of a line",
                options.show_markers == Some(true),
                &editor,
                |chart, on| chart.options.show_markers = on.then_some(true),
            ))
            .child(Self::row(
                "Aspect ratio",
                Self::dropdown_button(
                    "chart-aspect",
                    aspect_label(options.aspect_ratio),
                    &self.aspect_ratio,
                ),
            ))
            .child(Self::row(
                "Text size",
                Self::dropdown_button(
                    "chart-text-size",
                    text_size_label(options.text_size),
                    &self.text_size,
                ),
            ))
            .child(
                div().pt_2().child(
                    Button::new("chart-remove", "Remove chart")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChartEditorEvent::Remove))),
                ),
            )
    }
}
