//! The code behind the selected row of a trace: what to look up in the row, who looks it up, and
//! the section of Row Details that lists what was found.
//!
//! The lookup itself is `kusto_code`'s. This module feeds it the row, keeps it off the main
//! thread, and opens a place it found.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use editor::Editor;
use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{
    App, ClickEvent, Context, Entity, FontWeight, Modifiers, SharedString, Task, WeakEntity,
    Window, px,
};
use gpui_util::ResultExt as _;
use kusto_code::{
    CodeKeys, CodeRef, CodeRefKind, CodeResolver, EventText, GitSearch, KeyOrigin, SymbolPath,
    discover_repositories,
};
use kusto_results::inspector::{InspectorSubject, StackFrameLink, resolve_subject};
use kusto_results::trace_schema::{TraceColumns, matches_pattern};
use kusto_results::{ResultSet, Table};
use language::Point;
use project::ProjectPath;
use settings::Settings as _;
use ui::prelude::*;
use util::rel_path::RelPath;
use workspace::notifications::DetachAndPromptErr as _;
use workspace::{OpenOptions, OpenVisible, Pane, SplitDirection, Workspace};
use worktree::Worktree;

use crate::results_settings::ResultsSettings;

/// A result with more distinct markers than this is not worth looking all of them up ahead.
const MAX_PREFETCHED_MARKERS: usize = 500;

/// What the selected row says about its code. A message that only says something started or
/// ended (`structural_messages`) is the same on every row, so it says nothing about the code.
pub fn code_keys(
    table: &Table,
    columns: &TraceColumns,
    structural_messages: &[String],
    subject: &InspectorSubject,
) -> CodeKeys {
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
    let mut message = text_in(columns.message).filter(|message| {
        !structural_messages
            .iter()
            .any(|pattern| matches_pattern(pattern, message.trim()))
    });
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

/// The checkout a reference is in: its path without the part relative to the repository.
fn checkout_root(reference: &CodeRef) -> Option<PathBuf> {
    let relative = Path::new(&reference.relative_path);
    if relative.as_os_str().is_empty() || !reference.path.ends_with(relative) {
        return None;
    }
    reference
        .path
        .ancestors()
        .nth(relative.components().count())
        .map(Path::to_path_buf)
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

/// Where a place in the code is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenIn {
    /// The pane for code, beside the trace. It is found or made the first time and then used
    /// whichever pane has the focus, so the trace is never covered by the code it points to.
    CodePane,
    /// A new split beside the code pane, which then becomes the code pane.
    NewSplit,
    /// The pane that was last active, which is where Zed opens a file.
    ActivePane,
}

impl OpenIn {
    /// A plain click opens in the code pane, alt in a new split, and the secondary key (command
    /// on macOS) in the active pane. Any other combination, such as shift to extend a selection,
    /// is not a click on a link.
    pub fn from_modifiers(modifiers: &Modifiers) -> Option<Self> {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::none()
        };
        if *modifiers == Modifiers::none() {
            Some(Self::CodePane)
        } else if *modifiers == alt {
            Some(Self::NewSplit)
        } else if *modifiers == Modifiers::secondary_key() {
            Some(Self::ActivePane)
        } else {
            None
        }
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
    /// The pane code is opened in, once there is one.
    code_pane: Option<WeakEntity<Pane>>,
    /// The worktrees of the checkouts code was opened from. The project holds a worktree it did
    /// not open itself only weakly, and its language server would stop with the last tab.
    checkouts: Vec<Entity<Worktree>>,
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
            code_pane: None,
            checkouts: Vec::new(),
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
        let settings = ResultsSettings::get_global(cx);
        let columns = settings.trace_columns(table);
        let keys = code_keys(
            table,
            &columns,
            &settings.structural_messages,
            &resolve_subject(table, rows),
        );
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

    /// Opens the file a call stack frame names, found in the checkouts under the configured
    /// folders, at the frame's line. Says so when there is no such file.
    pub fn open_frame(
        &mut self,
        frame: &StackFrameLink,
        how: OpenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let roots = Self::roots(cx);
        if roots.is_empty() {
            self.show_problem(
                "Set kusto_results.code.roots to open the files of a call stack.".into(),
                cx,
            );
            return;
        }
        let resolver = self.resolver(roots, cx);
        let process = self
            .requested
            .as_ref()
            .and_then(|(keys, _)| keys.process.clone());
        let symbol = SymbolPath::parse(&frame.symbol);
        let (file_name, line) = (frame.file_name.clone(), frame.line);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let resolver = resolver.await?;
                    let found = resolver
                        .find_source_file(&file_name, line, symbol.as_ref(), process.as_deref())
                        .await;
                    Ok::<_, SharedString>((found, resolver.repositories().len(), file_name))
                })
                .await;
            this.update_in(cx, |this, window, cx| match outcome {
                Ok((found, repositories, file_name)) => match found.first() {
                    Some(first) => this.open(first, how, window, cx),
                    None => this.show_problem(
                        format!("{file_name} is not in any of the {repositories} checkouts."),
                        cx,
                    ),
                },
                Err(message) => this.show_problem(message.to_string(), cx),
            })
            .log_err();
        })
        .detach();
    }

    fn keep_checkout(&mut self, worktree: Entity<Worktree>) {
        if !self.checkouts.contains(&worktree) {
            self.checkouts.push(worktree);
        }
    }

    fn show_problem(&self, message: String, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_error(anyhow::anyhow!(message), cx)
        });
    }

    /// The pane to open code in, or `None` for the one Zed would use.
    fn pane_for(
        &mut self,
        how: OpenIn,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Option<Entity<Pane>> {
        let remembered = self
            .code_pane
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .filter(|pane| workspace.panes().contains(pane));
        let pane = match how {
            OpenIn::ActivePane => return None,
            OpenIn::CodePane => remembered.unwrap_or_else(|| {
                // Another pane that is already there, wherever it is, rather than a third.
                [
                    SplitDirection::Right,
                    SplitDirection::Left,
                    SplitDirection::Down,
                    SplitDirection::Up,
                ]
                .into_iter()
                .find_map(|direction| workspace.find_pane_in_direction(direction, cx))
                .unwrap_or_else(|| {
                    let active = workspace.active_pane().clone();
                    workspace.split_pane(active, SplitDirection::Right, window, cx)
                })
            }),
            OpenIn::NewSplit => {
                let beside = remembered.unwrap_or_else(|| workspace.active_pane().clone());
                workspace.split_pane(beside, SplitDirection::Right, window, cx)
            }
        };
        self.code_pane = Some(pane.downgrade());
        Some(pane)
    }

    /// Opens the file of a reference at its line, in the pane `how` says. Code in the code pane is
    /// a preview tab, so going through several places leaves one tab, and it takes the focus.
    pub fn open(
        &mut self,
        reference: &CodeRef,
        how: OpenIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let line = u32::try_from(reference.line.saturating_sub(1)).unwrap_or(u32::MAX);
        let path = reference.path.clone();
        let pane = workspace.update(cx, |workspace, cx| {
            self.pane_for(how, workspace, window, cx)
        });
        // The file is opened inside a worktree of its whole checkout, so that a language server
        // started for it sees the solution and can go to definitions and find references.
        let checkout = checkout_root(reference).map(|root| {
            workspace.update(cx, |workspace, cx| {
                workspace.project().update(cx, |project, cx| {
                    project.find_or_create_worktree(root, false, cx)
                })
            })
        });
        let workspace = workspace.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            // Without the checkout the file still opens, on its own.
            let mut in_checkout = None;
            if let Some(checkout) = checkout
                && let Some((worktree, _)) = checkout.await.log_err()
            {
                // The path is taken in the checkout's worktree rather than looked up, because a
                // worktree made earlier for the file alone, such as for a tab restored from the
                // last session, would otherwise be found first.
                in_checkout = worktree.read_with(cx, |worktree, _| {
                    let relative = path.strip_prefix(worktree.abs_path()).ok()?;
                    let relative = RelPath::new(relative, worktree.path_style()).log_err()?;
                    Some(ProjectPath {
                        worktree_id: worktree.id(),
                        path: relative.into_arc(),
                    })
                });
                this.update(cx, |this, _| this.keep_checkout(worktree))?;
            }
            let item = match (pane, in_checkout) {
                (None, Some(project_path)) => {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_path(project_path, None, true, window, cx)
                        })?
                        .await?
                }
                (None, None) => {
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_abs_path(
                                path,
                                OpenOptions {
                                    visible: Some(OpenVisible::None),
                                    ..Default::default()
                                },
                                window,
                                cx,
                            )
                        })?
                        .await?
                }
                (Some(pane), in_checkout) => {
                    // A worktree made for a file outside the project is only held weakly by the
                    // project, and goes away unless something keeps it until the file is open.
                    let (_worktree, project_path) = match in_checkout {
                        Some(project_path) => (None, project_path),
                        None => {
                            let project_path = workspace.update(cx, |workspace, cx| {
                                Workspace::project_path_for_path(
                                    workspace.project().clone(),
                                    &path,
                                    false,
                                    cx,
                                )
                            })?;
                            let (worktree, project_path) = project_path.await?;
                            (Some(worktree), project_path)
                        }
                    };
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_path_preview(
                                project_path,
                                Some(pane.downgrade()),
                                true,
                                true,
                                true,
                                window,
                                cx,
                            )
                        })?
                        .await?
                }
            };
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
fn heading(reference: &CodeRef) -> String {
    let heading = match (reference.origin, reference.kind) {
        (_, CodeRefKind::MarkerUse) => "Marker used in",
        (_, CodeRefKind::MarkerDefinition) => "Marker defined as",
        (_, CodeRefKind::LogLine) if reference.matches > 1 => {
            return format!("Message text in one of {} places", reference.matches);
        }
        (_, CodeRefKind::LogLine) => "Message logged at",
        (_, CodeRefKind::StackLine) => "Stack frame at",
        (KeyOrigin::Marker, _) => "Marker names",
        (KeyOrigin::Prefix, CodeRefKind::Method) => "Message names",
        (KeyOrigin::Prefix, _) => "Message names class",
        (KeyOrigin::Frame, CodeRefKind::Method) => "Stack frame",
        (KeyOrigin::Frame, _) => "Stack frame, class",
        (KeyOrigin::Message, _) => "Message names",
    };
    heading.to_string()
}

/// The name that stands out on a reference: for a line that uses a marker or logs a message, the
/// method it is in.
fn title(reference: &CodeRef) -> String {
    match (reference.kind, &reference.enclosing) {
        (CodeRefKind::MarkerUse | CodeRefKind::LogLine | CodeRefKind::StackLine, Some(method)) => {
            method.clone()
        }
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
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                if let Some(how) = OpenIn::from_modifiers(&event.modifiers()) {
                                    this.open(&opened, how, window, cx)
                                }
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

        // Most rows have a message to look for, so waiting and finding nothing take one line.
        let compact = match &self.state {
            CodeState::Looking => true,
            CodeState::Found { references, .. } => references.is_empty(),
            _ => false,
        };
        let section = v_flex().border_b_1().border_color(colors.border);
        if compact {
            return section.child(div().px_2().py_1().child(body));
        }
        section
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
        code_keys(table, &columns, &[], &resolve_subject(table, rows))
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
            matches: 1,
        }
    }

    #[test]
    fn a_use_of_a_marker_is_named_after_the_method_that_uses_it() {
        let used = reference(KeyOrigin::Marker, CodeRefKind::MarkerUse, Some("Charge"));
        assert_eq!(
            (heading(&used).as_str(), title(&used).as_str()),
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

    #[test]
    fn a_message_that_only_says_something_started_is_not_looked_for() {
        let table = Table {
            name: "t".into(),
            columns: vec![
                Column::new("MarkerName", "string"),
                Column::new("MessageText", "string"),
            ],
            rows: vec![
                vec![
                    Cell::Text("Orders-ChargeCard".into()),
                    Cell::Text("Monitored scope start Checkout.Charge for the order".into()),
                ],
                vec![
                    Cell::Text("Orders-ChargeCard".into()),
                    Cell::Text("Charging the card for the order".into()),
                ],
            ],
        };
        let columns = TraceColumns::detect(&table);
        let structural = vec!["Monitored scope start*".to_string()];
        let keys_of = |row: usize| {
            code_keys(
                &table,
                &columns,
                &structural,
                &resolve_subject(&table, &[row]),
            )
        };
        let scope_start = keys_of(0);
        assert_eq!(scope_start.marker.as_deref(), Some("Orders-ChargeCard"));
        assert_eq!(
            (scope_start.prefix, scope_start.literals),
            (None, Vec::new())
        );
        let logged = keys_of(1);
        assert_eq!(
            logged
                .literals
                .first()
                .and_then(|pieces| pieces.first())
                .map(String::as_str),
            Some("Charging the card for the order")
        );
    }

    #[test]
    fn a_log_line_is_named_after_its_method_and_says_when_the_text_is_in_several_places() {
        let mut logged = reference(KeyOrigin::Message, CodeRefKind::LogLine, Some("Fetch"));
        assert_eq!(
            (heading(&logged).as_str(), title(&logged).as_str()),
            ("Message logged at", "Fetch")
        );
        logged.matches = 3;
        assert_eq!(heading(&logged), "Message text in one of 3 places");
    }

    #[test]
    fn a_reference_is_opened_in_the_checkout_that_holds_it() {
        let mut found = reference(KeyOrigin::Marker, CodeRefKind::MarkerUse, None);
        assert_eq!(checkout_root(&found), Some(PathBuf::from("/code/orders")));

        found.relative_path = "Checkout.cs".into();
        found.path = PathBuf::from("/code/orders/Checkout.cs");
        assert_eq!(checkout_root(&found), Some(PathBuf::from("/code/orders")));

        found.relative_path = "src/Other.cs".into();
        assert_eq!(checkout_root(&found), None);
        found.relative_path = String::new();
        assert_eq!(checkout_root(&found), None);
    }
}
