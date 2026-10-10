//! The code behind the selected row of a trace: what to look up in the row, who looks it up, and
//! the section of Row Details that lists what was found.
//!
//! The lookup itself is `kusto_code`'s. This module feeds it the row, keeps it off the main
//! thread, and opens a place it found.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use editor::Editor;
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{App, Context, FontWeight, SharedString, Task, WeakEntity, Window, px};
use gpui_util::ResultExt as _;
use kusto_code::{
    CodeKeys, CodeRef, CodeRefKind, CodeResolver, EventText, GitSearch, KeyOrigin,
    discover_repositories,
};
use kusto_results::inspector::{InspectorSubject, resolve_subject};
use kusto_results::trace_schema::TraceColumns;
use kusto_results::{ResultSet, Table};
use language::Point;
use settings::Settings as _;
use ui::prelude::*;
use workspace::notifications::DetachAndPromptErr as _;
use workspace::{OpenOptions, OpenVisible, Workspace};

use crate::results_settings::ResultsSettings;

/// A result with more distinct markers than this is not worth looking all of them up ahead.
const MAX_PREFETCHED_MARKERS: usize = 500;

/// What the selected row says about its code.
pub fn code_keys(table: &Table, columns: &TraceColumns, subject: &InspectorSubject) -> CodeKeys {
    let row = match subject {
        InspectorSubject::Empty => return CodeKeys::default(),
        InspectorSubject::Row { row } => *row,
        InspectorSubject::FirstOfMany { first_row, .. } => *first_row,
        InspectorSubject::Assembled { rows, .. } => match rows.first() {
            Some(row) => *row,
            None => return CodeKeys::default(),
        },
    };
    let text_in = |column: Option<usize>| {
        column
            .and_then(|column| table.cell(row, column))
            .filter(|cell| !cell.is_null())
            .map(|cell| cell.display_text().into_owned())
            .filter(|text| !text.is_empty())
    };

    let marker = text_in(columns.marker);
    let process = text_in(columns.actor);
    let mut message = text_in(columns.message);
    // A call stack can be in any column, so the whole row is searched for frames.
    let mut details = (0..table.columns.len())
        .filter_map(|column| text_in(Some(column)))
        .collect::<Vec<_>>()
        .join("\n");
    if let InspectorSubject::Assembled { fields, .. } = subject {
        for field in fields {
            if Some(field.column) == columns.message {
                message = Some(field.text.clone());
            }
            details.push('\n');
            details.push_str(&field.text);
        }
    }

    CodeKeys::from_event(&EventText {
        marker: marker.as_deref(),
        message: message.as_deref(),
        details: Some(&details),
        process: process.as_deref(),
    })
}

/// The distinct markers of a table, so that all of them can be looked up at once.
fn distinct_markers(table: &Table, columns: &TraceColumns) -> Vec<String> {
    let Some(column) = columns.marker else {
        return Vec::new();
    };
    let markers: BTreeSet<String> = (0..table.rows.len())
        .filter_map(|row| table.cell(row, column))
        .filter(|cell| !cell.is_null())
        .map(|cell| cell.display_text().trim().to_string())
        .filter(|marker| !marker.is_empty())
        .collect();
    markers.into_iter().take(MAX_PREFETCHED_MARKERS).collect()
}

/// What the section says about the selected row.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeState {
    /// Nothing in the selection names code, so there is no section.
    Idle,
    /// The row names code, but no folder of checkouts is set.
    NotConfigured,
    Looking,
    Found {
        references: Vec<CodeRef>,
        repositories: usize,
    },
    Failed(String),
}

impl CodeState {
    pub fn is_shown(&self) -> bool {
        *self != CodeState::Idle
    }
}

type SharedResolver = Shared<Task<Result<Arc<CodeResolver>, SharedString>>>;

pub struct CodeReferences {
    workspace: Option<WeakEntity<Workspace>>,
    state: CodeState,
    /// What the current state answers, so that asking again does nothing.
    requested: Option<(CodeKeys, Vec<PathBuf>)>,
    roots: Vec<PathBuf>,
    resolver: Option<SharedResolver>,
    prefetched: Option<Weak<ResultSet>>,
    prefetch: Option<Shared<Task<()>>>,
    lookup: Option<Task<()>>,
}

impl CodeReferences {
    pub fn new() -> Self {
        Self {
            workspace: None,
            state: CodeState::Idle,
            requested: None,
            roots: Vec::new(),
            resolver: None,
            prefetched: None,
            prefetch: None,
            lookup: None,
        }
    }

    /// Without a workspace the shell environment is not known, and a place cannot be opened.
    pub fn set_workspace(&mut self, workspace: WeakEntity<Workspace>) {
        self.workspace = Some(workspace);
    }

    pub fn state(&self) -> &CodeState {
        &self.state
    }

    /// Follows the selection: finds the code behind the first selected row of `table_index`.
    pub fn select(
        &mut self,
        result: Option<&Arc<ResultSet>>,
        table_index: usize,
        rows: &[usize],
        cx: &mut Context<Self>,
    ) {
        let Some((result, table)) =
            result.and_then(|result| Some((result, result.tables.get(table_index)?)))
        else {
            self.show(CodeKeys::default(), cx);
            return;
        };
        let columns = ResultsSettings::get_global(cx).trace_columns(table);
        let keys = code_keys(table, &columns, &resolve_subject(table, rows));
        self.prefetch_markers(result, table, &columns, cx);
        self.show(keys, cx);
    }

    fn roots(cx: &App) -> Vec<PathBuf> {
        ResultsSettings::get_global(cx).code_roots.clone()
    }

    fn show(&mut self, keys: CodeKeys, cx: &mut Context<Self>) {
        let roots = Self::roots(cx);
        let request = (keys, roots);
        // A failure is tried again the next time the row is looked at.
        let failed = matches!(self.state, CodeState::Failed(_));
        if self.requested.as_ref() == Some(&request) && !failed {
            return;
        }
        let (keys, roots) = request.clone();
        self.requested = Some(request);
        self.lookup = None;

        if keys.is_empty() {
            self.state = CodeState::Idle;
        } else if roots.is_empty() {
            self.state = CodeState::NotConfigured;
        } else {
            self.state = CodeState::Looking;
            let resolver = self.resolver(roots, cx);
            let prefetch = self.prefetch.clone();
            self.lookup = Some(cx.spawn(async move |this, cx| {
                let outcome = cx
                    .background_spawn(async move {
                        let resolver = resolver.await?;
                        if let Some(prefetch) = prefetch {
                            prefetch.await;
                        }
                        let references = resolver.resolve(&keys).await;
                        Ok::<_, SharedString>((references, resolver.repositories().len()))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.state = match outcome {
                        Ok((references, repositories)) => CodeState::Found {
                            references,
                            repositories,
                        },
                        Err(message) => {
                            this.resolver = None;
                            CodeState::Failed(message.to_string())
                        }
                    };
                    cx.notify();
                })
                .log_err();
            }));
        }
        cx.notify();
    }

    /// Looks up the literals of all the markers of a result in one go, the first time one of
    /// its rows is selected, so that the lookups after it find them ready.
    fn prefetch_markers(
        &mut self,
        result: &Arc<ResultSet>,
        table: &Table,
        columns: &TraceColumns,
        cx: &mut Context<Self>,
    ) {
        let roots = Self::roots(cx);
        let this_result = Arc::downgrade(result);
        let already = self
            .prefetched
            .as_ref()
            .is_some_and(|prefetched| Weak::ptr_eq(prefetched, &this_result));
        if roots.is_empty() || already {
            return;
        }
        self.prefetched = Some(this_result);
        let markers = distinct_markers(table, columns);
        if markers.is_empty() {
            return;
        }
        let resolver = self.resolver(roots, cx);
        self.prefetch = Some(
            cx.background_spawn(async move {
                if let Some(resolver) = resolver.await.log_err() {
                    resolver.prefetch_markers(&markers).await;
                }
            })
            .shared(),
        );
    }

    /// The resolver for these roots, made on first use and again when the roots change.
    fn resolver(&mut self, roots: Vec<PathBuf>, cx: &mut Context<Self>) -> SharedResolver {
        if self.roots == roots
            && let Some(resolver) = &self.resolver
        {
            return resolver.clone();
        }
        let environment = self.workspace.as_ref().and_then(|workspace| {
            workspace
                .update(cx, |workspace, cx| {
                    let environment = workspace.project().read(cx).environment().clone();
                    environment.update(cx, |environment, cx| environment.default_environment(cx))
                })
                .log_err()
        });
        let resolver = cx
            .background_spawn({
                let roots = roots.clone();
                async move {
                    let environment = match environment {
                        Some(environment) => environment.await.unwrap_or_default(),
                        None => Default::default(),
                    };
                    let search = GitSearch::locate(environment)
                        .map_err(|error| SharedString::from(format!("{error:#}")))?;
                    let repositories = discover_repositories(&roots);
                    if repositories.is_empty() {
                        let folders = roots
                            .iter()
                            .map(|root| root.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(SharedString::from(format!(
                            "There are no git checkouts in {folders}"
                        )));
                    }
                    Ok(Arc::new(CodeResolver::new(search, repositories)))
                }
            })
            .shared();
        self.roots = roots;
        self.resolver = Some(resolver.clone());
        resolver
    }

    /// Opens the file of a reference at its line.
    pub fn open(&mut self, reference: &CodeRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let line = u32::try_from(reference.line.saturating_sub(1)).unwrap_or(u32::MAX);
        let opened = workspace.update(cx, |workspace, cx| {
            workspace.open_abs_path(
                reference.path.clone(),
                OpenOptions {
                    visible: Some(OpenVisible::None),
                    ..Default::default()
                },
                window,
                cx,
            )
        });
        cx.spawn_in(window, async move |_, cx| {
            let item = opened.await?;
            if let Some(editor) = item.downcast::<Editor>() {
                editor.update_in(cx, |editor, window, cx| {
                    editor.go_to_singleton_buffer_point(Point::new(line, 0), window, cx)
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_prompt_err("Could not open the code", window, cx, |_, _, _| None);
    }
}

/// What a reference is, as the label above it says.
fn heading(reference: &CodeRef) -> &'static str {
    match (reference.origin, reference.kind) {
        (_, CodeRefKind::MarkerUse) => "Marker used in",
        (_, CodeRefKind::MarkerDefinition) => "Marker defined as",
        (KeyOrigin::Marker, _) => "Marker names",
        (KeyOrigin::Prefix, CodeRefKind::Method) => "Message names",
        (KeyOrigin::Prefix, _) => "Message names class",
        (KeyOrigin::Frame, CodeRefKind::Method) => "Stack frame",
        (KeyOrigin::Frame, _) => "Stack frame, class",
    }
}

/// The name that stands out on a reference: for a use of a marker, the method that uses it.
fn title(reference: &CodeRef) -> String {
    match (reference.kind, &reference.enclosing) {
        (CodeRefKind::MarkerUse, Some(method)) => method.clone(),
        _ => reference.symbol.clone(),
    }
}

impl Render for CodeReferences {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let hover_background = colors.element_hover;
        let note = |text: String, color: Color| {
            Label::new(text)
                .size(LabelSize::Small)
                .color(color)
                .into_any_element()
        };
        let (count, body) = match &self.state {
            CodeState::Idle => (None, div().into_any_element()),
            CodeState::NotConfigured => (
                None,
                note(
                    "Set kusto_results.code.roots to find the code behind this row.".into(),
                    Color::Muted,
                ),
            ),
            CodeState::Looking => (None, note("Looking for code…".into(), Color::Muted)),
            CodeState::Failed(message) => (None, note(message.clone(), Color::Error)),
            CodeState::Found {
                references,
                repositories,
            } if references.is_empty() => (
                None,
                note(
                    format!("Nothing found in {repositories} repositories."),
                    Color::Muted,
                ),
            ),
            CodeState::Found { references, .. } => (
                Some(references.len()),
                div()
                    .id("row-details-code")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .children(references.iter().enumerate().map(|(index, reference)| {
                        let opened = reference.clone();
                        v_flex()
                            .id(("code-reference", index))
                            .px_2()
                            .py_1()
                            .gap_0p5()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover_background))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open(&opened, window, cx)
                            }))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Label::new(heading(reference))
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        Label::new(title(reference))
                                            .weight(FontWeight::SEMIBOLD)
                                            .truncate(),
                                    ),
                            )
                            .child(
                                Label::new(format!(
                                    "{} · {}:{}",
                                    reference.repository, reference.relative_path, reference.line
                                ))
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                            )
                            .child(
                                Label::new(reference.text.clone())
                                    .buffer_font(cx)
                                    .size(LabelSize::Small)
                                    .truncate(),
                            )
                    }))
                    .into_any_element(),
            ),
        };

        v_flex()
            .border_b_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .px_2()
                    .pt_2()
                    .gap_1()
                    .child(Label::new("Code").weight(FontWeight::SEMIBOLD))
                    .children(count.map(|count| Label::new(count.to_string()).color(Color::Muted))),
            )
            .child(div().px_2().pb_2().child(body))
    }
}

#[cfg(test)]
mod tests {
    use kusto_results::inspector::assemble_multipart;
    use kusto_results::{Cell, Column};

    use super::*;

    fn table() -> Table {
        Table {
            name: "PrimaryResult".into(),
            columns: ["MarkerName", "MessageText", "ProcessName", "CustomData"]
                .into_iter()
                .map(|name| Column::new(name, "string"))
                .collect(),
            rows: vec![
                vec![
                    Cell::Text("Orders-ChargeCard".into()),
                    Cell::Text("Checkout.Charge: charging".into()),
                    Cell::Text("Contoso.Orders.Host".into()),
                    Cell::Text(
                        r#"{"callStack":"   at Contoso.Orders.Ledger.Post(Entry entry)\r\n   at System.Threading.Tasks.Task.Wait()"}"#
                            .into(),
                    ),
                ],
                vec![
                    Cell::Text("Orders-ChargeCard".into()),
                    Cell::Null,
                    Cell::Null,
                    Cell::Null,
                ],
                vec![
                    Cell::Text("1/2: Checkout.Run starts and says ".into()),
                    Cell::Text("1/2: Checkout.Run starts and says ".into()),
                    Cell::Null,
                    Cell::Null,
                ],
                vec![
                    Cell::Text("Direct-Literal".into()),
                    Cell::Text("2/2: more".into()),
                    Cell::Null,
                    Cell::Null,
                ],
            ],
        }
    }

    fn keys_of(table: &Table, rows: &[usize]) -> CodeKeys {
        let columns = TraceColumns::detect(table);
        code_keys(table, &columns, &resolve_subject(table, rows))
    }

    #[test]
    fn a_row_offers_its_marker_its_prefix_its_process_and_the_frames_of_its_stack() {
        let keys = keys_of(&table(), &[0]);
        assert_eq!(keys.marker.as_deref(), Some("Orders-ChargeCard"));
        assert_eq!(keys.process.as_deref(), Some("Contoso.Orders.Host"));
        assert_eq!(
            keys.prefix.as_ref().map(|prefix| prefix.display()),
            Some("Checkout.Charge".to_string())
        );
        assert_eq!(
            keys.frames
                .iter()
                .map(|frame| frame.display())
                .collect::<Vec<_>>(),
            ["Ledger.Post"]
        );
    }

    #[test]
    fn null_cells_and_no_selection_offer_nothing_but_what_is_there() {
        let keys = keys_of(&table(), &[1]);
        assert_eq!(keys.marker.as_deref(), Some("Orders-ChargeCard"));
        assert_eq!(
            (keys.prefix, keys.frames.len(), keys.process),
            (None, 0, None)
        );
        assert!(keys_of(&table(), &[]).is_empty());
        assert!(keys_of(&table(), &[99]).is_empty());
    }

    #[test]
    fn the_first_row_of_several_is_the_one_looked_up() {
        let keys = keys_of(&table(), &[1, 0]);
        assert_eq!(keys.marker.as_deref(), Some("Orders-ChargeCard"));
        assert_eq!(keys.prefix, None);
    }

    #[test]
    fn a_message_assembled_from_parts_is_read_whole() {
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("MarkerName", "string"),
                Column::new("MessageText", "string"),
            ],
            rows: vec![
                vec![
                    Cell::Text("M".into()),
                    Cell::Text("1/2:Checkout.Charge: first half of a long ".into()),
                ],
                vec![Cell::Text("M".into()), Cell::Text("2/2:message".into())],
            ],
        };
        assert!(!assemble_multipart(&table, &[0, 1]).is_empty());
        let keys = keys_of(&table, &[0, 1]);
        assert_eq!(
            keys.prefix.as_ref().map(|prefix| prefix.display()),
            Some("Checkout.Charge".to_string())
        );
    }

    #[test]
    fn the_distinct_markers_of_a_table_are_sorted_and_without_blanks() {
        let table = table();
        let columns = TraceColumns::detect(&table);
        assert_eq!(
            distinct_markers(&table, &columns),
            [
                "1/2: Checkout.Run starts and says",
                "Direct-Literal",
                "Orders-ChargeCard"
            ]
        );
        let without_marker = TraceColumns::default();
        assert!(distinct_markers(&table, &without_marker).is_empty());
    }

    fn reference(origin: KeyOrigin, kind: CodeRefKind, enclosing: Option<&str>) -> CodeRef {
        CodeRef {
            origin,
            kind,
            repository: "orders".into(),
            path: PathBuf::from("/code/orders/src/Checkout.cs"),
            relative_path: "src/Checkout.cs".into(),
            line: 9,
            symbol: "CodeMarkers.ChargeCard".into(),
            enclosing: enclosing.map(str::to_string),
            text: "using (new MonitoredScope(CodeMarkers.ChargeCard))".into(),
        }
    }

    #[test]
    fn a_use_of_a_marker_is_named_after_the_method_that_uses_it() {
        let used = reference(KeyOrigin::Marker, CodeRefKind::MarkerUse, Some("Charge"));
        assert_eq!(
            (heading(&used), title(&used).as_str()),
            ("Marker used in", "Charge")
        );
        let unplaced = reference(KeyOrigin::Marker, CodeRefKind::MarkerUse, None);
        assert_eq!(title(&unplaced), "CodeMarkers.ChargeCard");
        let defined = reference(KeyOrigin::Marker, CodeRefKind::MarkerDefinition, None);
        assert_eq!(heading(&defined), "Marker defined as");
    }

    #[test]
    fn each_way_of_naming_code_has_its_own_heading() {
        let heading_of = |origin, kind| heading(&reference(origin, kind, None));
        assert_eq!(
            heading_of(KeyOrigin::Prefix, CodeRefKind::Method),
            "Message names"
        );
        assert_eq!(
            heading_of(KeyOrigin::Prefix, CodeRefKind::Type),
            "Message names class"
        );
        assert_eq!(
            heading_of(KeyOrigin::Frame, CodeRefKind::Method),
            "Stack frame"
        );
        assert_eq!(
            heading_of(KeyOrigin::Frame, CodeRefKind::Type),
            "Stack frame, class"
        );
        assert_eq!(
            heading_of(KeyOrigin::Marker, CodeRefKind::Method),
            "Marker names"
        );
    }
}
