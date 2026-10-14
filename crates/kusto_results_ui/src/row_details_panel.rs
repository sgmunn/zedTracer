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

use crate::code_references::{CodeReferences, OpenIn};
use crate::results_settings::ResultsSettings;

use crate::inspector_text::{InspectorEvent, InspectorPalette, InspectorText};
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
    code: Entity<CodeReferences>,
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
            let workspace = cx.weak_entity();
            cx.new(|cx| {
                let mut panel = Self::new(fs, window, cx);
                panel.set_workspace(workspace, cx);
                panel
            })
        })
    }

    /// Lets the code section open a place it found, and look up with the project's environment.
    pub fn set_workspace(&mut self, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) {
        self.code
            .update(cx, |code, _| code.set_workspace(workspace));
    }

    pub fn new(fs: Arc<dyn Fs>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let selection = ActiveSelection::shared(cx);
        let inspector = cx.new(|cx| InspectorText::new(window, cx));
        let find_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Find in row", window, cx);
            editor
        });
        let code = cx.new(|_| CodeReferences::new());
        let subscriptions = vec![
            cx.observe(&code, |_, _, cx| cx.notify()),
            cx.observe_in(&selection, window, |this, _, window, cx| {
                this.refresh(window, cx)
            }),
            cx.observe_global_in::<theme::GlobalTheme>(window, |this, window, cx| {
                // The colours were chosen when the document was shown; show it again in the
                // new theme.
                this.shown = InspectorDocument::default();
                this.refresh(window, cx);
            }),
            cx.subscribe_in(&inspector, window, |this, _, event, window, cx| {
                let InspectorEvent::OpenFrame { frame, modifiers } = event;
                if let Some(how) = OpenIn::from_modifiers(modifiers) {
                    this.code
                        .update(cx, |code, cx| code.open_frame(frame, how, window, cx));
                }
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
            code,
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
            link: theme.colors().text_accent,
        }
    }

    fn find_colour(cx: &App) -> Hsla {
        cx.theme().colors().search_match_background
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection.read(cx);
        let (result, table_index, rows) = (
            selection.result.clone(),
            selection.table_index,
            selection.rows.clone(),
        );
        self.code.update(cx, |code, cx| {
            code.select(result.as_ref(), table_index, &rows, cx)
        });
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
        Some(IconName::RowDetails)
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
            .when(self.code.read(cx).state().is_shown(), |panel| {
                panel.child(self.code.clone())
            })
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
    use kusto_code::CodeRefKind;
    use kusto_results::{Cell, Column, Table};
    use project::Project;
    use workspace::{AppState, Workspace};

    use super::*;
    use crate::code_references::CodeState;
    use workspace::SplitDirection;

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

    /// A change of theme restyles what the open panel already shows.
    #[gpui::test]
    async fn a_theme_change_restyles_the_shown_row(cx: &mut TestAppContext) {
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
        active.update(cx, |active, cx| {
            active.result = Some(exception_result());
            active.table_index = 0;
            active.rows = vec![0];
            cx.notify();
        });
        cx.run_until_parked();
        let header_colour = |cx: &mut gpui::VisualTestContext| {
            let editor =
                panel.read_with(cx, |panel, cx| panel.inspector().read(cx).editor().clone());
            editor.update(cx, |editor, cx| {
                editor
                    .text_highlights(
                        editor::display_map::HighlightKey::ConsoleAnsiHighlight(5),
                        cx,
                    )
                    .and_then(|(style, _)| style.color)
            })
        };
        let before = header_colour(cx);
        assert!(before.is_some());

        cx.update(|_, cx| {
            let mut changed = (**theme::GlobalTheme::theme(cx)).clone();
            changed.styles.colors.text_accent = gpui::hsla(0.5, 1., 0.5, 1.);
            theme::GlobalTheme::update_theme(cx, std::sync::Arc::new(changed));
        });
        cx.run_until_parked();
        let after = header_colour(cx);
        assert_eq!(after, Some(gpui::hsla(0.5, 1., 0.5, 1.)));
        assert_ne!(before, after);
    }

    const CHECKOUT: &str = "namespace Contoso.Orders
{
    public static class CodeMarkers
    {
        public const string ChargeCard = \"Orders-ChargeCard\";
    }

    public class Checkout
    {
        public void Charge()
        {
            this.Log($\"Charging the card for order {this.orderId}\");
            using (new MonitoredScope(CodeMarkers.ChargeCard))
            {
            }
        }
    }
}
";

    const LEDGER: &str = "namespace Contoso.Orders
{
    public class Ledger
    {
        public void Post()
        {
            this.Write();
        }
    }
}
";

    fn charge_card_result() -> Arc<ResultSet> {
        charge_card_result_named(["MarkerName", "MessageText", "ProcessName"])
    }

    /// The same rows under the names of the marker, message and process columns.
    fn charge_card_result_named(names: [&str; 3]) -> Arc<ResultSet> {
        Arc::new(ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns: names
                    .into_iter()
                    .map(|name| Column::new(name, "string"))
                    .collect(),
                rows: vec![
                    vec![
                        Cell::Text("Orders-ChargeCard".into()),
                        Cell::Text("Checkout.Charge: Charging the card for order 12345".into()),
                        Cell::Text("Contoso.Orders.Host".into()),
                    ],
                    vec![Cell::Null, Cell::Text("ok".into()), Cell::Null],
                    vec![
                        Cell::Null,
                        Cell::Text("Words that no checkout holds anywhere".into()),
                        Cell::Null,
                    ],
                ],
            }],
            ..Default::default()
        })
    }

    fn set_user_settings(settings: serde_json::Value, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(&settings.to_string(), cx)
                    .expect("the user settings parse");
            });
        });
    }

    fn set_code_roots(roots: &[&std::path::Path], cx: &mut gpui::VisualTestContext) {
        set_user_settings(
            serde_json::json!({ "kusto_results": { "code": { "roots": roots } } }),
            cx,
        );
    }

    /// Makes `repository` a git checkout holding `src/Checkout.cs`.
    fn checkout_with_the_source(repository: &std::path::Path) {
        std::fs::create_dir_all(repository.join("src")).expect("a folder");
        std::fs::write(repository.join("src/Checkout.cs"), CHECKOUT).expect("a file");
        std::fs::write(repository.join("src/Ledger.cs"), LEDGER).expect("a file");
        for arguments in [["init", "-q"], ["add", "-A"]] {
            let status = futures::executor::block_on(
                util::command::new_command("git")
                    .arg("-C")
                    .arg(repository)
                    .args(arguments)
                    .status(),
            )
            .expect("git runs");
            assert!(status.success(), "git {arguments:?}");
        }
    }

    fn select_row(
        active: &Entity<ActiveSelection>,
        row: usize,
        cx: &mut gpui::VisualTestContext,
    ) {
        select_row_of(active, charge_card_result(), row, cx);
    }

    fn select_row_of(
        active: &Entity<ActiveSelection>,
        result: Arc<ResultSet>,
        row: usize,
        cx: &mut gpui::VisualTestContext,
    ) {
        active.update(cx, |active, cx| {
            active.result = Some(result);
            active.table_index = 0;
            active.rows = vec![row];
            cx.notify();
        });
        cx.run_until_parked();
    }

    /// The lookup runs git on real threads, which the test executor does not wait for.
    fn finished_state(
        panel: &Entity<RowDetailsPanel>,
        cx: &mut gpui::VisualTestContext,
    ) -> CodeState {
        for _ in 0..400 {
            let state = panel.read_with(cx, |panel, cx| panel.code.read(cx).state().clone());
            if !matches!(state, CodeState::Looking) {
                return state;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
            cx.run_until_parked();
        }
        panic!("the lookup did not finish");
    }

    async fn panel_in_a_workspace(
        fs: Arc<FakeFs>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<RowDetailsPanel>,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(|cx| {
            AppState::test(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
        });
        let project = Project::test(fs.clone(), [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let panel = cx.update(|window, cx| {
            cx.new(|cx| {
                let mut panel = RowDetailsPanel::new(fs.clone(), window, cx);
                panel.set_workspace(workspace.downgrade(), cx);
                panel
            })
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.add_panel(panel.clone(), window, cx);
            workspace.focus_panel::<RowDetailsPanel>(window, cx);
        });
        (workspace, panel, cx)
    }

    /// Selecting a row lists the code its marker and message name in the checkouts of
    /// `kusto_results.code.roots`, and opening one goes to its line.
    #[gpui::test]
    async fn lists_the_code_behind_the_selected_row_and_opens_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = tempfile::tempdir().expect("a temporary directory");
        let repository = root.path().join("orders");
        checkout_with_the_source(&repository);

        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            repository.join("src"),
            serde_json::json!({ "Checkout.cs": CHECKOUT }),
        )
        .await;
        let (workspace, panel, cx) = panel_in_a_workspace(fs, cx).await;
        set_code_roots(&[root.path()], cx);

        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        select_row(&active, 0, cx);
        let CodeState::Found {
            references,
            repositories,
        } = finished_state(&panel, cx)
        else {
            panic!("the code was not found");
        };
        assert_eq!(repositories, 1);
        assert_eq!(
            references
                .iter()
                .map(|reference| (reference.kind, reference.line, reference.enclosing.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (CodeRefKind::LogLine, 12, Some("Charge")),
                (CodeRefKind::MarkerUse, 13, Some("Charge")),
                (CodeRefKind::MarkerDefinition, 5, None),
                (CodeRefKind::Method, 10, None),
            ]
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let code = panel.read_with(cx, |panel, _| panel.code.clone());
        let first = references.first().expect("a reference").clone();
        cx.update(|window, cx| {
            code.update(cx, |code, cx| {
                code.open(&first, OpenIn::CodePane, window, cx)
            })
        });
        cx.run_until_parked();
        let editor = workspace
            .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
            .expect("the file opened in an editor");
        let (text, cursor_row) = editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            (
                editor.text(cx),
                editor.selections.newest::<language::Point>(&snapshot).head().row,
            )
        });
        assert_eq!(text, CHECKOUT);
        assert_eq!(cursor_row, 11, "line 12, counting from zero");

        select_row(&active, 1, cx);
        assert_eq!(
            panel.read_with(cx, |panel, cx| panel.code.read(cx).state().clone()),
            CodeState::Idle,
            "a row that names no code has no section"
        );

        select_row(&active, 2, cx);
        assert_eq!(
            finished_state(&panel, cx),
            CodeState::Found {
                references: Vec::new(),
                repositories: 1
            },
            "text that no checkout holds is searched for and not found"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// The columns the code is looked up by are the ones the settings name, so a trace that calls
    /// them `ActivityType`, `EventText` and `Host` is searched once the setting says so.
    #[gpui::test]
    async fn looks_the_code_up_in_the_columns_the_settings_name(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = tempfile::tempdir().expect("a temporary directory");
        checkout_with_the_source(&root.path().join("orders"));
        let (_workspace, panel, cx) = panel_in_a_workspace(FakeFs::new(cx.executor()), cx).await;
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        let other_names = || charge_card_result_named(["ActivityType", "EventText", "Host"]);
        let state = |cx: &mut gpui::VisualTestContext| {
            panel.read_with(cx, |panel, cx| panel.code.read(cx).state().clone())
        };

        set_code_roots(&[root.path()], cx);
        select_row_of(&active, other_names(), 0, cx);
        assert_eq!(
            state(cx),
            CodeState::Idle,
            "none of those columns is a built-in name, so the row names no code"
        );

        set_user_settings(
            serde_json::json!({ "kusto_results": {
                "code": { "roots": [root.path()] },
                "trace_columns": {
                    "marker": ["MarkerName", "ActivityType"],
                    "message": ["MessageText", "EventText"],
                    "actor": ["ProcessName", "Host"]
                }
            } }),
            cx,
        );
        select_row_of(&active, other_names(), 0, cx);
        let CodeState::Found { references, .. } = finished_state(&panel, cx) else {
            panic!("the code was not found");
        };
        let kinds: Vec<CodeRefKind> = references.iter().map(|reference| reference.kind).collect();
        assert_eq!(
            kinds,
            [
                CodeRefKind::LogLine,
                CodeRefKind::MarkerUse,
                CodeRefKind::MarkerDefinition,
                CodeRefKind::Method
            ]
        );
    }

    /// Clicking the file of a call stack frame in the inspector opens it, at the frame's line, from
    /// the checkout under the configured folder.
    #[gpui::test]
    async fn a_click_on_the_file_of_a_call_stack_frame_opens_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = tempfile::tempdir().expect("a temporary directory");
        let repository = root.path().join("orders");
        checkout_with_the_source(&repository);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            repository.join("src"),
            serde_json::json!({ "Checkout.cs": CHECKOUT }),
        )
        .await;
        let (workspace, panel, cx) = panel_in_a_workspace(fs, cx).await;
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        set_code_roots(&[root.path()], cx);

        let stack = r"   at Contoso.Orders.Checkout.Charge() in C:\__w\1\s\src\Orders\Checkout.cs:line 13
   at System.Threading.Tasks.Task.Wait()";
        let result = Arc::new(ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns: vec![
                    Column::new("MessageText", "string"),
                    Column::new("CustomData", "dynamic"),
                ],
                rows: vec![vec![
                    Cell::Text("ok".into()),
                    Cell::Text(serde_json::json!({ "callStack": stack }).to_string()),
                ]],
            }],
            ..Default::default()
        });
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        select_row_of(&active, result, 0, cx);
        // The section of code above the inspector changes height when its lookup finishes, which
        // would move the link between the press and the release of a click.
        finished_state(&panel, cx);
        cx.run_until_parked();

        let (inspector, shown) = panel.read_with(cx, |panel, _| {
            (panel.inspector().clone(), panel.shown.clone())
        });
        let frame = shown
            .frames
            .first()
            .expect("a frame that names a file")
            .clone();
        assert_eq!(frame.file_name, "Checkout.cs");
        let position = crate::inspector_text::point_over(&inspector, cx, |offset| {
            frame.range.contains(&offset)
        });
        cx.simulate_click(position, gpui::Modifiers::none());

        let mut opened = None;
        for _ in 0..400 {
            cx.run_until_parked();
            opened =
                workspace.read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx));
            if opened.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let editor = opened.expect("the file opened in an editor");
        let (text, cursor_row) = editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            (
                editor.text(cx),
                editor
                    .selections
                    .newest::<language::Point>(&snapshot)
                    .head()
                    .row,
            )
        });
        assert_eq!(text, CHECKOUT);
        assert_eq!(cursor_row, 12, "line 13, counting from zero");
    }

    /// The file names on the tabs of every pane, left to right.
    fn tabs(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> Vec<Vec<String>> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .panes()
                .iter()
                .map(|pane| {
                    pane.read(cx)
                        .items()
                        .map(|item| {
                            // A file outside the project is titled with its whole path.
                            let title = item.tab_content_text(0, cx);
                            title.rsplit('/').next().unwrap_or_default().to_string()
                        })
                        .collect()
                })
                .collect()
        })
    }

    /// Waits for the tabs to be `expected`, because opening a file takes a few turns of the
    /// executor and a lookup that runs git takes real time.
    fn wait_for_tabs(
        workspace: &Entity<Workspace>,
        expected: &[&[&str]],
        cx: &mut gpui::VisualTestContext,
    ) {
        let expected: Vec<Vec<String>> = expected
            .iter()
            .map(|pane| pane.iter().map(|tab| tab.to_string()).collect())
            .collect();
        for _ in 0..400 {
            cx.run_until_parked();
            if tabs(workspace, cx) == expected {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(tabs(workspace, cx), expected);
    }

    fn click_frame(
        panel: &Entity<RowDetailsPanel>,
        index: usize,
        modifiers: gpui::Modifiers,
        cx: &mut gpui::VisualTestContext,
    ) {
        let (inspector, frames) = panel.read_with(cx, |panel, _| {
            (panel.inspector().clone(), panel.shown.frames.clone())
        });
        let range = frames.get(index).expect("a frame").range.clone();
        let position =
            crate::inspector_text::point_over(&inspector, cx, |offset| range.contains(&offset));
        cx.simulate_click(position, modifiers);
    }

    fn active_pane_index(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, _| {
            let active = workspace.active_pane();
            workspace
                .panes()
                .iter()
                .position(|pane| pane == active)
                .expect("the active pane is one of the panes")
        })
    }

    /// A trace in one tab and a call stack in Row Details that names two files.
    async fn trace_beside_a_call_stack(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<RowDetailsPanel>,
        tempfile::TempDir,
        &mut gpui::VisualTestContext,
    ) {
        cx.executor().allow_parking();
        let root = tempfile::tempdir().expect("a temporary directory");
        let repository = root.path().join("orders");
        checkout_with_the_source(&repository);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            repository.join("src"),
            serde_json::json!({ "Checkout.cs": CHECKOUT, "Ledger.cs": LEDGER }),
        )
        .await;
        fs.insert_tree("/trace", serde_json::json!({ "trace.txt": "the trace" }))
            .await;
        let (workspace, panel, cx) = panel_in_a_workspace(fs, cx).await;
        cx.simulate_resize(gpui::size(px(900.), px(700.)));
        set_code_roots(&[root.path()], cx);
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(
                    std::path::PathBuf::from("/trace/trace.txt"),
                    workspace::OpenOptions {
                        visible: Some(workspace::OpenVisible::None),
                        ..Default::default()
                    },
                    window,
                    cx,
                )
            })
            .await
            .expect("the trace opens");

        let stack = r"   at Contoso.Orders.Checkout.Charge() in C:\\__w\\1\\s\\src\\Checkout.cs:line 13
   at Contoso.Orders.Ledger.Post() in C:\\__w\\1\\s\\src\\Ledger.cs:line 7";
        let result = Arc::new(ResultSet {
            tables: vec![Table {
                name: "PrimaryResult".into(),
                columns: vec![
                    Column::new("MessageText", "string"),
                    Column::new("CustomData", "dynamic"),
                ],
                rows: vec![vec![
                    Cell::Text("ok".into()),
                    Cell::Text(serde_json::json!({ "callStack": stack }).to_string()),
                ]],
            }],
            ..Default::default()
        });
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        select_row_of(&active, result, 0, cx);
        finished_state(&panel, cx);
        cx.run_until_parked();
        (workspace, panel, root, cx)
    }

    /// A click on the file of a frame opens it in a pane beside the trace, and the next one uses
    /// that pane whichever pane has the focus, so the trace is never covered.
    #[gpui::test]
    async fn code_opens_beside_the_trace_and_keeps_using_that_pane(cx: &mut TestAppContext) {
        let (workspace, panel, _root, cx) = trace_beside_a_call_stack(cx).await;
        wait_for_tabs(&workspace, &[&["trace.txt"]], cx);

        click_frame(&panel, 0, gpui::Modifiers::none(), cx);
        wait_for_tabs(&workspace, &[&["trace.txt"], &["Checkout.cs"]], cx);
        assert_eq!(
            active_pane_index(&workspace, cx),
            1,
            "the focus goes to the code"
        );
        let editor = workspace
            .read_with(cx, |workspace, cx| workspace.active_item_as::<Editor>(cx))
            .expect("the code is in an editor");
        assert!(cx.update(|window, cx| editor.focus_handle(cx).is_focused(window)));

        // Back to the trace's pane, and another click: the code still goes to the code pane, and
        // replaces the preview tab instead of adding to it.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_pane_in_direction(SplitDirection::Left, window, cx)
        });
        assert_eq!(active_pane_index(&workspace, cx), 0);
        click_frame(&panel, 1, gpui::Modifiers::none(), cx);
        wait_for_tabs(&workspace, &[&["trace.txt"], &["Ledger.cs"]], cx);
        assert_eq!(active_pane_index(&workspace, cx), 1);
    }

    #[gpui::test]
    async fn the_keys_held_choose_the_pane(cx: &mut TestAppContext) {
        let (workspace, panel, _root, cx) = trace_beside_a_call_stack(cx).await;
        click_frame(&panel, 0, gpui::Modifiers::none(), cx);
        wait_for_tabs(&workspace, &[&["trace.txt"], &["Checkout.cs"]], cx);

        // The secondary key (command on macOS) opens where Zed would: the pane last active.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_pane_in_direction(SplitDirection::Left, window, cx)
        });
        click_frame(&panel, 1, gpui::Modifiers::secondary_key(), cx);
        wait_for_tabs(
            &workspace,
            &[&["trace.txt", "Ledger.cs"], &["Checkout.cs"]],
            cx,
        );

        // Alt opens in a new split beside the code pane, which is the code pane from then on.
        let alt = gpui::Modifiers {
            alt: true,
            ..gpui::Modifiers::none()
        };
        click_frame(&panel, 0, alt, cx);
        wait_for_tabs(
            &workspace,
            &[
                &["trace.txt", "Ledger.cs"],
                &["Checkout.cs"],
                &["Checkout.cs"],
            ],
            cx,
        );
        click_frame(&panel, 1, gpui::Modifiers::none(), cx);
        wait_for_tabs(
            &workspace,
            &[
                &["trace.txt", "Ledger.cs"],
                &["Checkout.cs"],
                &["Ledger.cs"],
            ],
            cx,
        );

        // Shift is for extending a selection, not for opening anything.
        click_frame(&panel, 0, gpui::Modifiers::shift(), cx);
        cx.run_until_parked();
        assert_eq!(tabs(&workspace, cx).len(), 3);
        assert_eq!(tabs(&workspace, cx)[2], ["Ledger.cs"]);
    }

    #[gpui::test]
    async fn a_closed_code_pane_is_made_again(cx: &mut TestAppContext) {
        let (workspace, panel, _root, cx) = trace_beside_a_call_stack(cx).await;
        click_frame(&panel, 0, gpui::Modifiers::none(), cx);
        wait_for_tabs(&workspace, &[&["trace.txt"], &["Checkout.cs"]], cx);

        let code_pane = workspace.read_with(cx, |workspace, _| workspace.panes()[1].clone());
        let closing = code_pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(&workspace::CloseActiveItem::default(), window, cx)
        });
        closing.await.expect("it closes");
        wait_for_tabs(&workspace, &[&["trace.txt"]], cx);

        click_frame(&panel, 1, gpui::Modifiers::none(), cx);
        wait_for_tabs(&workspace, &[&["trace.txt"], &["Ledger.cs"]], cx);
    }

    #[gpui::test]
    async fn says_how_to_turn_the_code_section_on(cx: &mut TestAppContext) {
        let (_workspace, panel, cx) = panel_in_a_workspace(FakeFs::new(cx.executor()), cx).await;
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        select_row(&active, 0, cx);
        assert_eq!(
            panel.read_with(cx, |panel, cx| panel.code.read(cx).state().clone()),
            CodeState::NotConfigured
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    #[gpui::test]
    async fn names_a_folder_that_holds_no_checkouts(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let root = tempfile::tempdir().expect("a temporary directory");
        let (_workspace, panel, cx) = panel_in_a_workspace(FakeFs::new(cx.executor()), cx).await;
        set_code_roots(&[root.path()], cx);
        let active = cx.update(|_, cx| ActiveSelection::shared(cx));
        select_row(&active, 0, cx);
        let CodeState::Failed(message) = finished_state(&panel, cx) else {
            panic!("an empty folder is not a success");
        };
        assert!(
            message.starts_with("There are no git checkouts in ")
                && message.contains(&root.path().display().to_string()),
            "{message}"
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // Cloning a repository there and looking at the row again finds it.
        let repository = root.path().join("orders");
        std::fs::create_dir_all(&repository).expect("a folder");
        std::fs::write(repository.join("Checkout.cs"), CHECKOUT).expect("a file");
        for arguments in [["init", "-q"], ["add", "-A"]] {
            futures::executor::block_on(
                util::command::new_command("git")
                    .arg("-C")
                    .arg(&repository)
                    .args(arguments)
                    .status(),
            )
            .expect("git runs");
        }
        select_row(&active, 0, cx);
        assert!(
            matches!(
                finished_state(&panel, cx),
                CodeState::Found { repositories: 1, .. }
            ),
            "a failure is not remembered"
        );
    }
}
