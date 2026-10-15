//! The pane that edits a chart: what it draws, how it is titled and how its axes look (CED).
//!
//! The editor changes a copy of the chart and reports each change; the view that hosts it
//! redraws, and the viewer saves. Only the options that can be edited here are shown. The rest
//! are kept by the chart as they were.

use editor::{Editor, EditorEvent};
use gpui::{
    Anchor, AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, SharedString, Subscription, WeakEntity, Window, actions, div, px,
};
use gpui_util::ResultExt as _;
use kusto_results::chart::{
    Aggregation, AspectRatio, AxisScale, LegendPosition, SortOrder, TextSize,
};
use kusto_results::{Chart, ChartType};
use serde_json::{Number, Value};
use ui::{
    Button, ContextMenu, DropdownMenu, PopoverMenuHandle, SwitchField, ToggleState, prelude::*,
};

actions!(
    chart_editor,
    [
        /// Moves to the next field of the chart editor.
        FocusNextField,
        /// Moves to the previous field of the chart editor.
        FocusPreviousField,
    ]
);

pub(crate) enum ChartEditorEvent {
    Changed(Chart),
    Remove,
    /// The user asked to put the editor away.
    Close,
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
    /// The editor's own focus handle, made a tab stop, so Tab reaches the field.
    focus: FocusHandle,
    _subscription: Subscription,
}

pub(crate) struct ChartEditor {
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    /// One for each row of the pane, so the row that takes focus can be scrolled into view.
    row_handles: Vec<FocusHandle>,
    _row_subscriptions: Vec<Subscription>,
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
    aggregation: Dropdown,
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
        let aggregation = Self::dropdown(
            std::iter::once((SharedString::from("Not combined"), None))
                .chain(Aggregation::ALL.iter().map(|aggregation| {
                    (SharedString::from(aggregation.label()), Some(*aggregation))
                }))
                .collect(),
            |chart, aggregation: Option<Aggregation>| chart.options.aggregation = aggregation,
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
            scroll_handle: ScrollHandle::new(),
            row_handles: Vec::new(),
            _row_subscriptions: Vec::new(),
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
            aggregation,
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
        let focus = editor.focus_handle(cx).tab_stop(true);
        Input {
            editor,
            focus,
            _subscription,
        }
    }

    /// Puts the cursor in the first field, so the pane can be used without the mouse.
    pub(crate) fn focus_first(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.name.focus, cx);
    }

    /// Makes a handle, and a subscription that scrolls the pane to its row, for each row up to
    /// `count`.
    fn ensure_rows(&mut self, count: usize, window: &mut Window, cx: &mut Context<Self>) {
        while self.row_handles.len() < count {
            let index = self.row_handles.len();
            let handle = cx.focus_handle();
            self._row_subscriptions
                .push(cx.on_focus_in(&handle, window, move |this, _, _| {
                    this.scroll_handle.scroll_to_item(index)
                }));
            self.row_handles.push(handle);
        }
    }

    #[cfg(test)]
    pub(crate) fn edit(&mut self, change: impl FnOnce(&mut Chart), cx: &mut Context<Self>) {
        change(&mut self.chart);
        self.changed(cx);
    }

    #[cfg(test)]
    pub(crate) fn y_max_editor(&self) -> Entity<Editor> {
        self.y_max.editor.clone()
    }

    #[cfg(test)]
    pub(crate) fn name_focus(&self) -> FocusHandle {
        self.name.focus.clone()
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

    fn row(label: &'static str, control: impl IntoElement) -> Div {
        h_flex()
            .justify_between()
            .items_center()
            .gap_2()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(control)
    }

    fn section(title: &'static str) -> Div {
        div().pt_1().child(
            Label::new(title)
                .size(LabelSize::Small)
                .weight(gpui::FontWeight::BOLD),
        )
    }

    fn dropdown_button(
        id: &'static str,
        label: impl Into<SharedString>,
        dropdown: &Dropdown,
    ) -> DropdownMenu {
        DropdownMenu::new(id, label, dropdown.menu.clone())
            .handle(dropdown.handle.clone())
            .attach(Anchor::BottomLeft)
            .tab_index(0)
    }

    fn input_box(input: &Input, window: &Window, cx: &App) -> Div {
        let colors = cx.theme().colors();
        let border = if input.focus.contains_focused(window, cx) {
            colors.border_focused
        } else {
            colors.border
        };
        div()
            .w(px(150.))
            .px_2()
            .py_0p5()
            .border_1()
            .border_color(border)
            .rounded_sm()
            .track_focus(&input.focus)
            .child(input.editor.clone())
    }

    fn switch(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        on: bool,
        editor: &WeakEntity<Self>,
        apply: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
    ) -> SwitchField {
        let editor = editor.clone();
        SwitchField::new(id, Some(label), None, state(on), move |new_state, _, cx| {
            let on = matches!(new_state, ToggleState::Selected);
            editor.update(cx, |this, cx| apply(this, on, cx)).log_err();
        })
        .tab_index(0)
    }

    fn option_switch(
        id: &'static str,
        label: &'static str,
        on: bool,
        editor: &WeakEntity<Self>,
        apply: impl Fn(&mut Chart, bool) + 'static,
    ) -> SwitchField {
        Self::switch(id, label, on, editor, move |this, on, cx| {
            apply(&mut this.chart, on);
            this.changed(cx);
        })
    }

    fn column_switch(
        id: (&'static str, usize),
        name: &SharedString,
        on: bool,
        editor: &WeakEntity<Self>,
        toggle: fn(&mut Self, &str, &mut Context<Self>),
    ) -> SwitchField {
        let column = name.clone();
        Self::switch(id, name.clone(), on, editor, move |this, _, cx| {
            toggle(this, &column, cx)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let selected = |chosen: &Option<Vec<String>>, effective: &[String], name: &str| match chosen
        {
            Some(names) if !names.is_empty() => names.iter().any(|chosen| chosen == name),
            _ => effective.iter().any(|chosen| chosen == name),
        };
        let value_switches: Vec<_> =
            self.columns
                .iter()
                .enumerate()
                .filter(|(_, column)| {
                    column.numeric
                        || options.y_columns.as_ref().is_some_and(|names| {
                            names.iter().any(|name| name == column.name.as_ref())
                        })
                })
                .map(|(index, column)| {
                    Self::column_switch(
                        ("chart-value-column", index),
                        &column.name,
                        selected(&options.y_columns, &self.effective.values, &column.name),
                        &editor,
                        Self::toggle_value_column,
                    )
                })
                .collect();
        let series_switches: Vec<_> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                Self::column_switch(
                    ("chart-series-column", index),
                    &column.name,
                    selected(
                        &options.series_columns,
                        &self.effective.series,
                        &column.name,
                    ),
                    &editor,
                    Self::toggle_series_column,
                )
            })
            .collect();
        let table_label = self.chart.table_name.clone().unwrap_or_default();
        let input = |input: &Input| Self::input_box(input, window, cx);

        // The rows, in the order they are on the screen, which is the order Tab goes in.
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(Self::section("Chart").into_any_element());
        rows.push(Self::row("Name", input(&self.name)).into_any_element());
        rows.push(
            Self::row(
                "Type",
                Self::dropdown_button("chart-type", type_label, &self.chart_type),
            )
            .into_any_element(),
        );
        if let Some(table) = &self.table {
            rows.push(
                Self::row(
                    "Table",
                    Self::dropdown_button("chart-table", table_label, table),
                )
                .into_any_element(),
            );
        }
        rows.push(Self::section("Data").into_any_element());
        rows.push(
            Self::row(
                "X column",
                Self::dropdown_button("chart-x-column", x_label, &self.x_column),
            )
            .into_any_element(),
        );
        rows.push(
            Label::new("Values (Y)")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        );
        rows.extend(
            value_switches
                .into_iter()
                .map(IntoElement::into_any_element),
        );
        rows.push(
            Label::new("Split into series by")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
        );
        rows.extend(
            series_switches
                .into_iter()
                .map(IntoElement::into_any_element),
        );
        rows.push(Self::row("Most series", input(&self.max_series)).into_any_element());
        rows.push(
            Self::option_switch(
                "chart-accumulate",
                "Running total",
                options.accumulate == Some(true),
                &editor,
                |chart, on| chart.options.accumulate = on.then_some(true),
            )
            .into_any_element(),
        );
        rows.push(
            Self::row(
                "Sort",
                Self::dropdown_button("chart-sort", sort_label(options.sort), &self.sort),
            )
            .into_any_element(),
        );
        rows.push(
            Self::row(
                "Rows at the same X",
                Self::dropdown_button(
                    "chart-aggregation",
                    options
                        .aggregation
                        .map_or("Not combined", Aggregation::label),
                    &self.aggregation,
                ),
            )
            .into_any_element(),
        );
        rows.push(Self::section("Titles").into_any_element());
        rows.push(Self::row("Title", input(&self.title)).into_any_element());
        rows.push(Self::row("X axis", input(&self.x_title)).into_any_element());
        rows.push(Self::row("Y axis", input(&self.y_title)).into_any_element());
        rows.push(Self::section("Axes").into_any_element());
        rows.push(
            Self::row(
                "X scale",
                Self::dropdown_button("chart-x-scale", scale_label(options.x_axis), &self.x_scale),
            )
            .into_any_element(),
        );
        rows.push(Self::row("X from", input(&self.x_min)).into_any_element());
        rows.push(Self::row("X to", input(&self.x_max)).into_any_element());
        rows.push(
            Self::row(
                "Y scale",
                Self::dropdown_button("chart-y-scale", scale_label(options.y_axis), &self.y_scale),
            )
            .into_any_element(),
        );
        rows.push(Self::row("Y from", input(&self.y_min)).into_any_element());
        rows.push(Self::row("Y to", input(&self.y_max)).into_any_element());
        rows.push(
            Self::option_switch(
                "chart-x-grid",
                "X gridlines",
                options.grid_shown(true),
                &editor,
                |chart, on| chart.options.x_show_grid = Some(on),
            )
            .into_any_element(),
        );
        rows.push(
            Self::option_switch(
                "chart-y-grid",
                "Y gridlines",
                options.grid_shown(false),
                &editor,
                |chart, on| chart.options.y_show_grid = Some(on),
            )
            .into_any_element(),
        );
        rows.push(Self::section("Look").into_any_element());
        rows.push(
            Self::row(
                "Legend",
                Self::dropdown_button(
                    "chart-legend",
                    legend_label(options.legend_position),
                    &self.legend,
                ),
            )
            .into_any_element(),
        );
        rows.push(
            Self::option_switch(
                "chart-show-values",
                "Show values on bars",
                options.show_values == Some(true),
                &editor,
                |chart, on| chart.options.show_values = on.then_some(true),
            )
            .into_any_element(),
        );
        rows.push(
            Self::option_switch(
                "chart-show-markers",
                "Mark the points of a line",
                options.show_markers == Some(true),
                &editor,
                |chart, on| chart.options.show_markers = on.then_some(true),
            )
            .into_any_element(),
        );
        rows.push(
            Self::row(
                "Aspect ratio",
                Self::dropdown_button(
                    "chart-aspect",
                    aspect_label(options.aspect_ratio),
                    &self.aspect_ratio,
                ),
            )
            .into_any_element(),
        );
        rows.push(
            Self::row(
                "Text size",
                Self::dropdown_button(
                    "chart-text-size",
                    text_size_label(options.text_size),
                    &self.text_size,
                ),
            )
            .into_any_element(),
        );
        rows.push(
            div()
                .pt_2()
                .child(
                    Button::new("chart-remove", "Remove chart")
                        .tab_index(0i16)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChartEditorEvent::Remove))),
                )
                .into_any_element(),
        );

        self.ensure_rows(rows.len(), window, cx);
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                div()
                    .id(("chart-editor-row", index))
                    .track_focus(&self.row_handles[index])
                    .child(row)
            })
            .collect();

        v_flex()
            .id("chart-editor")
            .debug_selector(|| "chart-editor".to_string())
            .key_context("ChartEditor")
            .track_focus(&self.focus_handle)
            .track_scroll(&self.scroll_handle)
            .on_action(cx.listener(|_, _: &FocusNextField, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPreviousField, window, cx| window.focus_prev(cx)))
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(ChartEditorEvent::Close)))
            .w(px(300.))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .gap_2()
            .p_2()
            .border_l_1()
            .border_color(border)
            .children(rows)
    }
}
