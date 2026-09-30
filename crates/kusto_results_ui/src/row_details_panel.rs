//! The Row Details panel: a right-dock panel that follows the selection of whichever results
//! grid the user last selected in.

use std::sync::Arc;

use editor::Editor;
use editor::actions::Cancel;
use fs::Fs;
use gpui::{
    App, AppContext as _, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Global, Hsla, Pixels, Subscription, WeakEntity, Window, actions, px,
};
use kusto_results::ResultSet;
use kusto_results::inspector::{
    InspectorDocument, InspectorSubject, JsonTokenKind, build_document, resolve_subject,
};
use settings::{DockSide, Settings as _};
use ui::{Button, ButtonCommon, ButtonSize, Clickable, IconName, Label, prelude::*};
use workspace::Workspace;

use crate::results_settings::ResultsSettings;

use crate::inspector_text::{InspectorPalette, InspectorText};
use workspace::dock::{DockPosition, Panel, PanelEvent};

actions!(row_details, [ToggleRowDetails, FocusFind]);

const EMPTY_STATE: &str = "Select a result row to inspect its values here.";

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
            return vec![EMPTY_STATE.to_string()];
        };
        match resolve_subject(table, &self.rows) {
            InspectorSubject::Empty => {
                vec![EMPTY_STATE.to_string()]
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
    fs: Arc<dyn Fs>,
    selection: Entity<ActiveSelection>,
    inspector: Entity<InspectorText>,
    find_editor: Entity<Editor>,
    /// What the inspector currently shows, so a repeated selection does not reset its scroll.
    shown: InspectorDocument,
    headline: Vec<String>,
    match_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl RowDetailsPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let fs = workspace.app_state().fs.clone();
            cx.new(|cx| Self::new(fs, window, cx))
        })
    }

    pub fn new(fs: Arc<dyn Fs>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let selection = ActiveSelection::shared(cx);
        let inspector = cx.new(|cx| InspectorText::new(window, cx));
        let find_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Find in row", window, cx);
            editor
        });
        let subscriptions = vec![
            cx.observe_in(&selection, window, |this, _, window, cx| {
                this.refresh(window, cx)
            }),
            cx.subscribe_in(&find_editor, window, |this, _, event, window, cx| {
                if matches!(event, editor::EditorEvent::BufferEdited) {
                    this.apply_find(window, cx);
                }
            }),
        ];
        let mut panel = Self {
            focus_handle: cx.focus_handle(),
            fs,
            selection,
            inspector,
            find_editor,
            shown: InspectorDocument::default(),
            headline: Vec::new(),
            match_count: 0,
            _subscriptions: subscriptions,
        };
        panel.refresh(window, cx);
        panel
    }

    pub fn headline(&self, cx: &App) -> Vec<String> {
        self.selection.read(cx).headline()
    }

    pub fn inspector(&self) -> &Entity<InspectorText> {
        &self.inspector
    }

    pub fn find_editor(&self) -> &Entity<Editor> {
        &self.find_editor
    }

    pub fn match_count(&self) -> usize {
        self.match_count
    }

    fn palette(cx: &App) -> InspectorPalette {
        let theme = cx.theme();
        let text = theme.colors().text;
        let syntax = theme.syntax().clone();
        let token = move |name: &str| {
            syntax
                .style_for_name(name)
                .and_then(|style| style.color)
                .unwrap_or(text)
        };
        let keys = token("property");
        let strings = token("string");
        let numbers = token("number");
        let booleans = token("boolean");
        let nulls = token("constant");
        InspectorPalette {
            token: Box::new(move |kind| match kind {
                JsonTokenKind::Key => keys,
                JsonTokenKind::String => strings,
                JsonTokenKind::Number => numbers,
                JsonTokenKind::Boolean => booleans,
                JsonTokenKind::Null => nulls,
            }),
            header: theme.colors().text_accent,
            null: theme.colors().text_muted,
            code_background: theme.colors().element_background,
        }
    }

    fn find_colour(cx: &App) -> Hsla {
        cx.theme().colors().search_match_background
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection.read(cx);
        let headline = selection.headline();
        let document = selection
            .result
            .as_ref()
            .and_then(|result| result.tables.get(selection.table_index))
            .map(|table| build_document(table, &resolve_subject(table, &selection.rows)))
            .unwrap_or_default();
        if headline != self.headline {
            self.headline = headline;
            cx.notify();
        }
        if document == self.shown {
            return;
        }
        let palette = Self::palette(cx);
        self.inspector.update(cx, |inspector, cx| {
            inspector.set_document(&document, &palette, window, cx)
        });
        self.shown = document;
        self.apply_find(window, cx);
        cx.notify();
    }

    fn apply_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.find_editor.read(cx).text(cx);
        let colour = Self::find_colour(cx);
        self.match_count = self.inspector.update(cx, |inspector, cx| {
            inspector.find(&query, colour, window, cx)
        });
        cx.notify();
    }

    fn focus_find(&mut self, _: &FocusFind, window: &mut Window, cx: &mut Context<Self>) {
        self.find_editor.update(cx, |editor, cx| {
            editor.select_all(&editor::actions::SelectAll, window, cx)
        });
        window.focus(&self.find_editor.focus_handle(cx), cx);
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        let find_focused = self.find_editor.focus_handle(cx).is_focused(window);
        if !find_focused || self.find_editor.read(cx).text(cx).is_empty() {
            cx.propagate();
            return;
        }
        self.find_editor
            .update(cx, |editor, cx| editor.clear(window, cx));
    }

    fn toggle_wrap(&mut self, cx: &mut Context<Self>) {
        self.inspector.update(cx, |inspector, cx| {
            let wrap = !inspector.wrap();
            inspector.set_wrap(wrap, cx);
        });
        cx.notify();
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

    fn position(&self, _: &Window, cx: &App) -> DockPosition {
        match ResultsSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.kusto_results.get_or_insert_default().dock = Some(dock);
        });
    }

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
        let has_subject = !self.shown.text.is_empty();
        let colors = cx.theme().colors();
        let wrap_label = if self.inspector.read(cx).wrap() {
            "Wrap lines: On"
        } else {
            "Wrap lines: Off"
        };
        let find_text_present = !self.find_editor.read(cx).text(cx).is_empty();
        let match_label = match (find_text_present, self.match_count) {
            (false, _) => None,
            (true, 0) => Some("No matches".to_string()),
            (true, 1) => Some("1 match".to_string()),
            (true, count) => Some(format!("{count} matches")),
        };

        let header = v_flex()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(colors.border)
            .children(self.headline.iter().enumerate().map(|(index, line)| {
                let label = Label::new(line.clone());
                if index == 0 && has_subject {
                    label.weight(gpui::FontWeight::SEMIBOLD).into_any_element()
                } else {
                    label.color(Color::Muted).into_any_element()
                }
            }))
            .when(has_subject, |header| {
                header
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.editor_background)
                            .child(self.find_editor.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("row-details-wrap", wrap_label)
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, _, cx| this.toggle_wrap(cx))),
                            )
                            .children(
                                match_label.map(|label| Label::new(label).color(Color::Muted)),
                            ),
                    )
            });

        v_flex()
            .key_context("RowDetailsPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_find))
            .on_action(cx.listener(Self::cancel))
            .size_full()
            .child(header)
            .when(has_subject, |panel| {
                panel.child(
                    div()
                        .id("row-details-body")
                        .flex_1()
                        .min_h_0()
                        .p_2()
                        .child(self.inspector.clone()),
                )
            })
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
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));

        let details = cx.update(|window, cx| {
            cx.new(|cx| {
                RowDetailsPanel::new(FakeFs::new(cx.background_executor().clone()), window, cx)
            })
        });
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

    fn exception_result() -> Arc<ResultSet> {
        Arc::new(ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns: vec![
                    Column::new("Message", "string"),
                    Column::new("Exception", "dynamic"),
                    Column::new("Trace", "string"),
                ],
                rows: vec![vec![
                    Cell::Text("Timeout talking to sql-01".into()),
                    Cell::Text(
                        "{\"type\":\"SqlException\",\"message\":\"Timeout expired\"}".into(),
                    ),
                    Cell::Null,
                ]],
            }],
            ..Default::default()
        })
    }

    /// RDT-3 to RDT-6 and RDT-12: field blocks, find, wrap, and a find text that survives.
    #[gpui::test]
    async fn shows_field_blocks_and_finds_in_them(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let (panel, cx) = cx.add_window_view(|window, cx| {
            RowDetailsPanel::new(FakeFs::new(cx.background_executor().clone()), window, cx)
        });
        cx.simulate_resize(gpui::size(px(360.), px(600.)));
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        let shown_text = |cx: &mut gpui::VisualTestContext| {
            panel.read_with(cx, |panel, cx| {
                panel.inspector().read(cx).editor().read(cx).text(cx)
            })
        };
        assert_eq!(shown_text(cx), "");

        active.update(cx, |active, cx| {
            active.result = Some(exception_result());
            active.table_index = 0;
            active.rows = vec![0];
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            shown_text(cx),
            "Message · string\nTimeout talking to sql-01\n\nException · dynamic\n{\n  \"type\": \"SqlException\",\n  \"message\": \"Timeout expired\"\n}\n\nTrace · string\nnull"
        );

        let inspector_editor =
            panel.read_with(cx, |panel, cx| panel.inspector().read(cx).editor().clone());
        assert!(
            inspector_editor.read_with(cx, |editor, _| editor
                .has_background_highlights(crate::inspector_text::CODE_BACKGROUND_KEY)),
            "a JSON value is set apart as code (JSN-2)"
        );
        let find_editor = panel.read_with(cx, |panel, _| panel.find_editor().clone());
        find_editor.update_in(cx, |editor, window, cx| {
            editor.set_text("TIMEOUT", window, cx)
        });
        cx.run_until_parked();
        assert_eq!(panel.read_with(cx, |panel, _| panel.match_count()), 2);

        active.update(cx, |active, cx| {
            active.rows = vec![0, 0];
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.match_count()),
            2,
            "the find text survives a selection change"
        );

        panel.update_in(cx, |panel, window, cx| {
            window.focus(&panel.find_editor().focus_handle(cx), cx);
        });
        cx.dispatch_action(Cancel);
        assert_eq!(find_editor.read_with(cx, |editor, cx| editor.text(cx)), "");
        assert_eq!(panel.read_with(cx, |panel, _| panel.match_count()), 0);

        panel.update(cx, |panel, cx| panel.toggle_wrap(cx));
        let wrapped = panel.read_with(cx, |panel, cx| panel.inspector().read(cx).wrap());
        assert!(!wrapped);

        panel.update_in(cx, |panel, window, cx| {
            window.focus(&panel.inspector().read(cx).editor().focus_handle(cx), cx);
        });
        cx.dispatch_action(FocusFind);
        let focused = panel.update_in(cx, |panel, window, cx| {
            panel.find_editor().focus_handle(cx).is_focused(window)
        });
        assert!(focused);

        active.update(cx, |active, cx| {
            *active = ActiveSelection::default();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(shown_text(cx), "");
    }

    /// RDT-10: the panel docks where the `kusto_results.dock` setting says, on either side.
    #[gpui::test]
    async fn the_panel_docks_on_the_side_the_setting_names(cx: &mut TestAppContext) {
        cx.update(|cx| {
            AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let (panel, cx) = cx.add_window_view(|window, cx| {
            RowDetailsPanel::new(FakeFs::new(cx.background_executor().clone()), window, cx)
        });
        let position = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| panel.read(cx).position(window, cx))
        };
        assert_eq!(position(cx), DockPosition::Right, "the default");
        assert!(panel.read_with(cx, |panel, _| {
            panel.position_is_valid(DockPosition::Left)
                && panel.position_is_valid(DockPosition::Right)
                && !panel.position_is_valid(DockPosition::Bottom)
        }));

        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(r#"{ "kusto_results": { "dock": "left" } }"#, cx)
                    .expect("the user settings parse");
            });
        });
        assert_eq!(position(cx), DockPosition::Left);
    }
}
