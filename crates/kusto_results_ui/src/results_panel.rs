//! The Results panel in the bottom dock: the output of the latest run.
//!
//! A run is saved to the history folder and the panel shows that file in the same viewer a
//! results tab uses, so what the panel shows can be loaded again from history.

use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Pixels, Render, SharedString, Styled as _, Window, actions, div, px,
};
use kusto_results::ResultSet;
use language::LanguageRegistry;
use ui::{Icon, IconName, Label, LabelCommon as _, LabelSize, prelude::*};
use workspace::Workspace;
use workspace::dock::{DockPosition, Panel, PanelEvent};

use crate::results_viewer::{ResultsFile, ResultsViewer};

actions!(
    kusto,
    [
        /// Shows or hides the Results panel.
        ToggleResults
    ]
);

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleResults, window, cx| {
        if !workspace.toggle_panel_focus::<ResultsPanel>(window, cx) {
            workspace.close_panel::<ResultsPanel>(window, cx);
        }
    });
}

enum Content {
    Empty,
    Result(Shown),
    Error(SharedString),
}

struct Shown {
    viewer: Entity<ResultsViewer>,
    rows: usize,
    summary: SharedString,
}

pub struct ResultsPanel {
    focus_handle: FocusHandle,
    content: Content,
}

impl ResultsPanel {
    pub async fn load(
        workspace: gpui::WeakEntity<Workspace>,
        mut cx: gpui::AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |_, _, cx| cx.new(Self::new))
    }

    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: Content::Empty,
        }
    }

    /// Shows the result in `file`, replacing whatever was shown before, an error included.
    pub(crate) fn show_result(
        &mut self,
        file: Entity<ResultsFile>,
        languages: Option<Arc<LanguageRegistry>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewer = cx.new(|cx| ResultsViewer::new(file, languages, window, cx));
        let result = viewer.read(cx).result(cx);
        self.content = Content::Result(Shown {
            viewer,
            rows: result.total_rows(),
            summary: summarize(&result).into(),
        });
        cx.notify();
    }

    /// Shows why a run failed, replacing whatever was shown before.
    pub(crate) fn show_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = Content::Error(message.into());
        cx.notify();
    }

    pub(crate) fn shown_viewer(&self) -> Option<&Entity<ResultsViewer>> {
        match &self.content {
            Content::Result(shown) => Some(&shown.viewer),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn shown_error(&self) -> Option<&str> {
        match &self.content {
            Content::Error(message) => Some(message),
            _ => None,
        }
    }
}

/// `help.kusto.windows.net / Samples · 1,240 rows · took 1.8 s · 10:42:11`
pub(crate) fn summarize(result: &ResultSet) -> String {
    let mut parts = Vec::new();
    let place: Vec<&str> = [result.cluster.as_deref(), result.database.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !place.is_empty() {
        parts.push(place.join(" / "));
    }
    let rows = result.total_rows();
    parts.push(format!("{rows} {}", if rows == 1 { "row" } else { "rows" }));
    if let Some(milliseconds) = result.execution_duration_ms {
        parts.push(format!("took {}", format_duration(milliseconds)));
    }
    if let Some(started) = result
        .execution_started_at
        .as_deref()
        .and_then(|started| DateTime::parse_from_rfc3339(started).ok())
    {
        parts.push(started.with_timezone(&Local).format("%H:%M:%S").to_string());
    }
    parts.join(" · ")
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1000 {
        format!("{milliseconds} ms")
    } else {
        format!("{:.1} s", milliseconds as f64 / 1000.0)
    }
}

impl EventEmitter<PanelEvent> for ResultsPanel {}

impl Focusable for ResultsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ResultsPanel {
    fn persistent_name() -> &'static str {
        "Kusto Results Panel"
    }

    fn panel_key() -> &'static str {
        "ResultsPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Bottom
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Bottom
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Table)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Results")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleResults)
    }

    /// The row count of the result shown, or `!` after a failed run.
    fn icon_label(&self, _: &Window, _: &App) -> Option<String> {
        match &self.content {
            Content::Empty => None,
            Content::Result(shown) => Some(shown.rows.to_string()),
            Content::Error(_) => Some("!".to_string()),
        }
    }

    fn activation_priority(&self) -> u32 {
        9
    }
}

impl Render for ResultsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let root = v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(colors.panel_background);
        match &self.content {
            Content::Empty => root.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Label::new("No results").color(Color::Muted)),
            ),
            Content::Error(message) => root.child(
                h_flex()
                    .p_3()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(IconName::XCircle).color(Color::Error))
                    .child(div().flex_1().child(Label::new(message.clone()))),
            ),
            Content::Result(shown) => root
                .child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(colors.border)
                        .child(
                            Label::new(shown.summary.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
                .child(div().flex_1().min_h_0().child(shown.viewer.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_where_the_result_came_from_and_how_long_it_took() {
        let result = ResultSet {
            cluster: Some("help.kusto.windows.net".into()),
            database: Some("Samples".into()),
            execution_duration_ms: Some(1840),
            ..ResultSet::default()
        };
        assert_eq!(
            summarize(&result),
            "help.kusto.windows.net / Samples · 0 rows · took 1.8 s"
        );
    }

    #[test]
    fn a_single_row_is_not_plural_and_a_short_run_is_in_milliseconds() {
        let mut result = ResultSet {
            execution_duration_ms: Some(42),
            ..ResultSet::default()
        };
        result.tables.push(kusto_results::Table {
            name: "t".into(),
            columns: vec![kusto_results::Column::new("a", "long")],
            rows: vec![vec![kusto_results::Cell::Int(1)]],
        });
        assert_eq!(summarize(&result), "1 row · took 42 ms");
    }
}
