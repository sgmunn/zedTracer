//! The Row Details panel: a right-dock panel that follows the selection of whichever results
//! grid the user last selected in.

use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Global, Pixels, Subscription,
    Window, actions, px,
};
use kusto_results::ResultSet;
use kusto_results::inspector::{InspectorSubject, resolve_subject};
use ui::{IconName, Label, prelude::*};
use workspace::dock::{DockPosition, Panel, PanelEvent};

actions!(row_details, [ToggleRowDetails]);

/// The selection the inspector follows: the rows last selected in any results grid.
#[derive(Default)]
pub struct ActiveSelection {
    pub result: Option<Arc<ResultSet>>,
    pub table_index: usize,
    /// Source rows in display order.
    pub rows: Vec<usize>,
}

struct ActiveSelectionGlobal(Entity<ActiveSelection>);

impl Global for ActiveSelectionGlobal {}

impl ActiveSelection {
    /// The one shared selection, created on first use.
    pub fn shared(cx: &mut App) -> Entity<ActiveSelection> {
        if let Some(existing) = cx.try_global::<ActiveSelectionGlobal>() {
            return existing.0.clone();
        }
        let entity = cx.new(|_| ActiveSelection::default());
        cx.set_global(ActiveSelectionGlobal(entity.clone()));
        entity
    }

    /// What the panel shows for this selection, as lines of text.
    pub fn headline(&self) -> Vec<String> {
        let Some(table) = self
            .result
            .as_ref()
            .and_then(|result| result.tables.get(self.table_index))
        else {
            return vec!["Select a result row to inspect its values here.".to_string()];
        };
        match resolve_subject(table, &self.rows) {
            InspectorSubject::Empty => {
                vec!["Select a result row to inspect its values here.".to_string()]
            }
            InspectorSubject::Row { row } => vec![
                table.name.clone(),
                format!("Row {} · {} fields", row + 1, table.columns.len()),
            ],
            InspectorSubject::Assembled { rows, fields } => {
                let mut lines = vec![
                    table.name.clone(),
                    format!(
                        "{} selected rows · multi-part message assembled",
                        rows.len()
                    ),
                ];
                lines.extend(fields.iter().map(|field| {
                    format!("{} merged {}-part message", field.column_name, field.total)
                }));
                lines
            }
            InspectorSubject::FirstOfMany {
                first_row,
                selected_count,
            } => vec![
                table.name.clone(),
                format!("Row {} · {} fields", first_row + 1, table.columns.len()),
                format!("{selected_count} rows selected - showing the first"),
            ],
        }
    }
}

pub struct RowDetailsPanel {
    focus_handle: FocusHandle,
    selection: Entity<ActiveSelection>,
    _subscription: Subscription,
}

impl RowDetailsPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let selection = ActiveSelection::shared(cx);
        let subscription = cx.observe(&selection, |_, _, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            selection,
            _subscription: subscription,
        }
    }

    pub fn headline(&self, cx: &App) -> Vec<String> {
        self.selection.read(cx).headline()
    }
}

impl EventEmitter<PanelEvent> for RowDetailsPanel {}

impl Focusable for RowDetailsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RowDetailsPanel {
    fn persistent_name() -> &'static str {
        "Row Details Panel"
    }

    fn panel_key() -> &'static str {
        "RowDetailsPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(360.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ListTree)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Row Details")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleRowDetails)
    }

    fn activation_priority(&self) -> u32 {
        8
    }
}

impl Render for RowDetailsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .p_3()
            .gap_1()
            .children(
                self.headline(cx)
                    .into_iter()
                    .map(|line| Label::new(line).into_any_element()),
            )
    }
}

#[cfg(test)]
mod tests {
    use fs::FakeFs;
    use gpui::TestAppContext;
    use investigation::InvestigationPanel;
    use kusto_results::{Cell, Column, Table};
    use project::Project;
    use workspace::{AppState, Workspace};

    use super::*;

    fn messages() -> Arc<ResultSet> {
        Arc::new(ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns: vec![
                    Column::new("Message", "string"),
                    Column::new("Level", "long"),
                ],
                rows: vec![
                    vec![Cell::Text("1/2:{\"a\":".into()), Cell::Int(4)],
                    vec![Cell::Text("2/2:1}".into()), Cell::Int(4)],
                    vec![Cell::Text("plain".into()), Cell::Int(2)],
                ],
            }],
            ..Default::default()
        })
    }

    /// S6: the panel docks on the right beside the Investigation panel, each can be shown in
    /// turn, and it follows the selection published by a grid.
    #[gpui::test]
    async fn shares_the_right_dock_and_follows_the_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let details = cx.update(|_, cx| cx.new(RowDetailsPanel::new));
        let window_cx = cx.update(|window, cx| window.to_async(cx));
        let investigation = InvestigationPanel::load(workspace.downgrade(), window_cx)
            .await
            .expect("the investigation panel loads");
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_panel(details.clone(), window, cx);
            workspace.add_panel(investigation.clone(), window, cx);
        });

        workspace.read_with(cx, |workspace, cx| {
            let dock = workspace.right_dock().read(cx);
            assert_eq!(dock.panels_len(), 2);
            assert!(dock.panel::<RowDetailsPanel>().is_some());
            assert!(dock.panel::<InvestigationPanel>().is_some());
        });

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_panel::<RowDetailsPanel>(window, cx);
        });
        let visible = |cx: &mut gpui::VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .right_dock()
                    .read(cx)
                    .visible_panel()
                    .map(|panel| panel.persistent_name())
            })
        };
        assert_eq!(visible(cx), Some("Row Details Panel"));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_panel::<InvestigationPanel>(window, cx);
        });
        assert_eq!(visible(cx), Some("Investigation Panel"));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_panel::<RowDetailsPanel>(window, cx);
        });
        assert_eq!(visible(cx), Some("Row Details Panel"));

        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        let headline = |cx: &mut gpui::VisualTestContext| {
            details.read_with(cx, |panel, cx| panel.headline(cx))
        };
        assert_eq!(
            headline(cx),
            ["Select a result row to inspect its values here."]
        );

        let select = |rows: Vec<usize>, cx: &mut gpui::VisualTestContext| {
            active.update(cx, |active, cx| {
                active.result = Some(messages());
                active.table_index = 0;
                active.rows = rows;
                cx.notify();
            });
        };
        select(vec![2], cx);
        assert_eq!(headline(cx), ["PrimaryResult", "Row 3 · 2 fields"]);
        select(vec![1, 0], cx);
        assert_eq!(
            headline(cx),
            [
                "PrimaryResult",
                "2 selected rows · multi-part message assembled",
                "Message merged 2-part message"
            ]
        );
        select(vec![0, 2], cx);
        assert_eq!(
            headline(cx),
            [
                "PrimaryResult",
                "Row 1 · 2 fields",
                "2 rows selected - showing the first"
            ]
        );
        select(Vec::new(), cx);
        assert_eq!(
            headline(cx),
            ["Select a result row to inspect its values here."]
        );
    }
}
