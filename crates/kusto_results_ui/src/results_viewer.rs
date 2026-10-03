//! Opening a `.ktt` or `.kqr` file in a tab: the project item that loads and parses it, and the
//! tab that shows its first table in a results grid.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use encoding_rs::Encoding;
use gpui::{
    Action as _, App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div,
};
use editor::Editor;
use menu::{Cancel, Confirm};
use gpui_util::ResultExt as _;
use kusto_results::activity::{Focus, FocusTarget, build_projection_in, resolve_focus};
use kusto_results::findings::{Finding, Findings, FindingsOptions};
use kusto_results::sequence::{SequenceOptions, build_sequence};
use kusto_results::timeline::{Timeline, TimelineOptions};
use kusto_results::waterfall::Waterfall;
use kusto_results::trace_schema::TraceRole;
use kusto_results::{NoResultData, ResultSet, TableView};
use project::{Project, ProjectEntryId, ProjectPath};
use rope::Rope;
use settings::Settings as _;
use text::LineEnding;
use ui::{Button, ButtonSize, Icon, IconName, prelude::*};
use util::rel_path::RelPath;
use workspace::Pane;
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ProjectItem as WorkspaceProjectItem};
use worktree::Worktree;

use crate::findings_strip::{FindingsStrip, FindingsStripEvent};
use crate::grid::{ResultGrid, ResultGridEvent};
use crate::query_view::{QueryView, parameter_values};
use crate::results_settings::ResultsSettings;
use crate::row_details_panel::{RowDetailsPanel, ToggleRowDetails};
use crate::run_query::RerunQuery;
use crate::sequence_view::SequenceView;
use crate::structured_view::{StructuredView, StructuredViewEvent};
use crate::waterfall_view::{WaterfallEvent, WaterfallView};

pub fn init(cx: &mut App) {
    workspace::register_project_item::<ResultsViewer>(cx);
    crate::run_query::share_defaults_with_language_server(cx);
    crate::history::prune_at_startup(cx);
    cx.observe_new(|workspace: &mut Workspace, _, cx| {
        crate::run_query::register(workspace, cx);
        crate::results_panel::register(workspace);
        crate::query_parameters::register(workspace);
        crate::history::register(workspace);
        crate::save_result::register(workspace);
        workspace.register_action(|workspace, _: &ToggleRowDetails, window, cx| {
            if !workspace.toggle_panel_focus::<RowDetailsPanel>(window, cx) {
                workspace.close_panel::<RowDetailsPanel>(window, cx);
            }
        });
    })
    .detach();
}

fn is_results_path(path: &RelPath) -> bool {
    matches!(path.extension(), Some("ktt" | "kqr"))
}

/// A parsed results file.
pub struct ResultsFile {
    project_path: ProjectPath,
    file_name: SharedString,
    result: Arc<ResultSet>,
    /// Why there is nothing to show, for a file that is not a usable result.
    problem: Option<&'static str>,
    /// How the file was stored, so that writing it back keeps its line endings and encoding.
    storage: Option<FileStorage>,
    save_delay: Option<Task<()>>,
    write_in_flight: Option<Task<()>>,
    changed_during_write: bool,
    reload_task: Option<Task<()>>,
    /// The project holds a worktree that is not shown only weakly, so a result opened from the
    /// history would lose its worktree, and with it reloading and writing its layout back.
    _worktree: Entity<Worktree>,
    _watch: Subscription,
}

/// A viewer shows the file again when its content was changed from outside.
pub enum ResultsFileEvent {
    Reloaded,
}

impl EventEmitter<ResultsFileEvent> for ResultsFile {}

/// Parses a result file, or says why it cannot be shown.
async fn parse(
    text: String,
    file_name: &SharedString,
    cx: &AsyncApp,
) -> (ResultSet, Option<&'static str>) {
    match cx
        .background_spawn(async move { ResultSet::from_json(&text) })
        .await
    {
        Ok(result) if result.tables.is_empty() => (result, Some(NO_RESULT_DATA)),
        Ok(result) => (result, None),
        Err(error) if error.is::<NoResultData>() => (ResultSet::default(), Some(NO_RESULT_DATA)),
        Err(error) => {
            log::warn!("{file_name} is not a result file: {error:#}");
            (ResultSet::default(), Some(INVALID_RESULT_FILE))
        }
    }
}

struct FileStorage {
    worktree: WeakEntity<Worktree>,
    line_ending: LineEnding,
    encoding: &'static Encoding,
    has_bom: bool,
}

impl FileStorage {
    fn new(loaded: &worktree::LoadedFile, worktree: &Entity<Worktree>) -> Option<Self> {
        loaded.is_writable.then(|| Self {
            worktree: worktree.downgrade(),
            line_ending: loaded.line_ending,
            encoding: loaded.encoding,
            has_bom: loaded.has_bom,
        })
    }
}

/// Layout changes are written after the user has stopped for this long.
const INVALID_RESULT_FILE: &str = "Invalid result file.";
const NO_RESULT_DATA: &str = "No result data found.";

const SAVE_DELAY: Duration = Duration::from_millis(300);

impl ResultsFile {
    /// Keeps a changed column layout in the result and writes it back to the file, where the
    /// file can be written. Only `tableViews` changes; everything else in the file is kept.
    fn save_layout(&mut self, layout: TableView, cx: &mut Context<Self>) {
        if self.result.table_view(&layout.name) == Some(&layout) {
            return;
        }
        let mut result = (*self.result).clone();
        result.set_table_view(layout);
        self.result = Arc::new(result);
        if self.storage.is_none() {
            return;
        }
        self.save_delay = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            this.update(cx, |this, cx| {
                this.save_delay = None;
                this.write(cx)
            })
            .log_err();
        }));
    }

    /// Reads the file again after a change on disk. Our own writes come back through here too,
    /// and are ignored because they hold what is already shown; so are changes that arrive
    /// while a layout of ours is waiting to be written, which would otherwise undo it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(worktree) = self
            .storage
            .as_ref()
            .and_then(|storage| storage.worktree.upgrade())
        else {
            return;
        };
        let path = self.project_path.path.clone();
        let file_name = self.file_name.clone();
        let load = worktree.update(cx, |worktree, cx| worktree.load_file(&path, cx));
        self.reload_task = Some(cx.spawn(async move |this, cx| {
            let loaded = match load.await {
                Ok(loaded) => loaded,
                Err(error) => {
                    log::warn!("could not read {file_name} again: {error:#}");
                    return;
                }
            };
            let (result, problem) = parse(loaded.text.to_string(), &file_name, cx).await;
            this.update(cx, |this, cx| {
                if this.save_delay.is_some() || this.write_in_flight.is_some() {
                    return;
                }
                if this.problem == problem && *this.result == result {
                    return;
                }
                this.result = Arc::new(result);
                this.problem = problem;
                if let Some(worktree) = this
                    .storage
                    .as_ref()
                    .map(|storage| storage.worktree.clone())
                    && let Some(worktree) = worktree.upgrade()
                {
                    this.storage = FileStorage::new(&loaded, &worktree);
                }
                cx.emit(ResultsFileEvent::Reloaded);
            })
            .log_err();
        }));
    }

    fn write(&mut self, cx: &mut Context<Self>) {
        if self.write_in_flight.is_some() {
            self.changed_during_write = true;
            return;
        }
        let Some(storage) = &self.storage else {
            return;
        };
        let text = match self.result.to_json() {
            Ok(text) => text,
            Err(error) => {
                log::error!("could not write {}: {error:#}", self.file_name);
                return;
            }
        };
        let write = storage.worktree.update(cx, |worktree, cx| {
            worktree.write_file(
                self.project_path.path.clone(),
                Rope::from(text.as_str()),
                storage.line_ending,
                storage.encoding,
                storage.has_bom,
                cx,
            )
        });
        let file_name = self.file_name.clone();
        let write = match write {
            Ok(write) => write,
            Err(error) => {
                log::error!("could not write {file_name}: {error:#}");
                return;
            }
        };
        self.write_in_flight = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = write.await {
                log::error!("could not write {file_name}: {error:#}");
            }
            this.update(cx, |this, cx| {
                this.write_in_flight = None;
                if std::mem::take(&mut this.changed_during_write) {
                    this.write(cx);
                }
            })
            .log_err();
        }));
    }
}

impl project::ProjectItem for ResultsFile {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Entity<Self>>>> {
        let worktree = project.read(cx).worktree_for_id(path.worktree_id, cx)?;
        let is_results_file = is_results_path(path.path.as_ref())
            || (path.path.as_ref() == RelPath::empty()
                && matches!(
                    worktree
                        .read(cx)
                        .abs_path()
                        .extension()
                        .and_then(OsStr::to_str),
                    Some("ktt" | "kqr")
                ));
        if !is_results_file {
            return None;
        }

        let project_path = path.clone();
        let file_name: SharedString = project
            .read(cx)
            .absolute_path(path, cx)
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Results".to_string())
            .into();
        let load = worktree.update(cx, |worktree, cx| worktree.load_file(&path.path, cx));
        Some(cx.spawn(async move |cx| {
            let started = std::time::Instant::now();
            let loaded = load.await.with_context(|| format!("reading {file_name}"))?;
            log::info!(
                "kusto: read {file_name} ({:.1} MB) in {:?}",
                loaded.text.len() as f64 / 1e6,
                started.elapsed()
            );
            let started = std::time::Instant::now();
            let (result, problem) = parse(loaded.text.to_string(), &file_name, cx).await;
            log::info!("kusto: parsed {file_name} in {:?}", started.elapsed());
            let storage = FileStorage::new(&loaded, &worktree);
            Ok(cx.new(|cx| {
                let watch = cx.subscribe(&worktree, |this: &mut Self, _, event, cx| {
                    if let worktree::Event::UpdatedEntries(changes) = event
                        && changes
                            .iter()
                            .any(|(path, _, _)| *path == this.project_path.path)
                    {
                        this.reload(cx);
                    }
                });
                Self {
                    project_path,
                    file_name,
                    result: Arc::new(result),
                    problem,
                    storage,
                    save_delay: None,
                    write_in_flight: None,
                    changed_during_write: false,
                    reload_task: None,
                    _worktree: worktree,
                    _watch: watch,
                }
            }))
        }))
    }

    fn entry_id(&self, _: &App) -> Option<ProjectEntryId> {
        None
    }

    fn project_path(&self, _: &App) -> Option<ProjectPath> {
        Some(self.project_path.clone())
    }

    fn is_dirty(&self) -> bool {
        false
    }
}

/// Which view of the table the tab shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Data,
    Structured,
    /// The calls between the actors of a trace, as a sequence diagram.
    Sequence,
    /// Where the time went, as rows of bars.
    Timeline,
    /// The query the result came from, to read.
    Query,
}

/// The structured view is built when it is first asked for: the projection of a large trace
/// takes a moment, and most tabs never need it.
enum Structured {
    Building { _task: Task<()> },
    Ready(Entity<StructuredView>),
}

/// The sequence diagram is built when it is first asked for, away from the window.
enum Sequence {
    Building { _task: Task<()> },
    Ready(Entity<SequenceView>),
}

/// The field an activity id is typed into to focus on it.
struct FocusInput {
    editor: Entity<Editor>,
}

/// What a focus is asked for by.
enum FocusRequest {
    Id(String),
    /// The activity that owns this source row.
    Row(usize),
}

/// The timeline is built when it is first asked for, away from the window.
enum TimelineState {
    Building { _task: Task<()> },
    Ready(Entity<WaterfallView>),
}

/// The findings are worked out when the result is first shown, away from the window.
enum FindingsState {
    Building { _task: Task<()> },
    Ready(Entity<FindingsStrip>),
}

pub struct ResultsViewer {
    focus_handle: FocusHandle,
    results_file: Entity<ResultsFile>,
    grid: Option<Entity<ResultGrid>>,
    problem: Option<&'static str>,
    mode: ViewMode,
    /// Whether the table has the columns the structured view needs.
    can_show_structured: bool,
    structured: Option<Structured>,
    /// Whether the table has the columns the sequence view needs.
    can_show_sequence: bool,
    /// For a trace the sequence view cannot draw: the parts of a trace it could not find.
    sequence_missing: Option<SharedString>,
    sequence: Option<Sequence>,
    /// How the diagram is drawn: the settings, changed by the controls of this tab.
    sequence_options: SequenceOptions,
    findings: Option<FindingsState>,
    _findings_subscription: Option<Subscription>,
    /// Whether the table has the columns the Timeline tab needs.
    can_show_timeline: bool,
    timeline_missing: Option<SharedString>,
    timeline: Option<TimelineState>,
    _timeline_subscription: Option<Subscription>,
    /// An activity to reveal in the tree once the structured view is built.
    pending_structured_reveal: Option<usize>,
    /// The activity every view is built from, with what is below it; `None` is the whole trace.
    focus: Option<Arc<Focus>>,
    /// The field a Focus… id is typed in, while it is open.
    focus_input: Option<FocusInput>,
    focus_note: Option<SharedString>,
    _focus_task: Option<Task<()>>,
    /// Whether rows that only say something started or ended are left out of the grids (TPL-1).
    hide_structural: bool,
    /// Whether the file says which query it came from, which can be shown and run again.
    can_show_query: bool,
    query_view: Option<Entity<QueryView>>,
    _grid_subscription: Option<Subscription>,
    _structured_subscription: Option<Subscription>,
    _structured_events_subscription: Option<Subscription>,
    _reload_subscription: Subscription,
}

impl EventEmitter<()> for ResultsViewer {}

impl Focusable for ResultsViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ResultsViewer {
    pub(crate) fn project_path(&self, cx: &App) -> ProjectPath {
        self.results_file.read(cx).project_path.clone()
    }
}

impl Item for ResultsViewer {
    type Event = ();

    fn tab_content_text(&self, _: usize, cx: &App) -> SharedString {
        self.results_file.read(cx).file_name.clone()
    }

    fn tab_icon(&self, _: &Window, _: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Table))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        callback: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        callback(self.results_file.entity_id(), self.results_file.read(cx));
    }

    fn buffer_kind(&self, _: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }
}

impl WorkspaceProjectItem for ResultsViewer {
    type Item = ResultsFile;

    fn for_project_item(
        _: Entity<Project>,
        _: Option<&Pane>,
        item: Entity<Self::Item>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(item, window, cx)
    }
}

impl ResultsViewer {
    /// A viewer of a result file, which shows it again when the file changes.
    pub(crate) fn new(
        item: Entity<ResultsFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reload = cx.subscribe_in(
            &item,
            window,
            |this, item, _: &ResultsFileEvent, window, cx| {
                this.show(item, window, cx);
                cx.notify();
            },
        );
        let mut viewer = Self {
            focus_handle: cx.focus_handle(),
            results_file: item.clone(),
            grid: None,
            problem: None,
            mode: ViewMode::Data,
            can_show_structured: false,
            structured: None,
            can_show_sequence: false,
            sequence_missing: None,
            sequence: None,
            sequence_options: SequenceOptions::default(),
            findings: None,
            _findings_subscription: None,
            can_show_timeline: false,
            timeline_missing: None,
            timeline: None,
            _timeline_subscription: None,
            pending_structured_reveal: None,
            focus: None,
            focus_input: None,
            focus_note: None,
            _focus_task: None,
            hide_structural: false,
            can_show_query: false,
            query_view: None,
            _grid_subscription: None,
            _structured_subscription: None,
            _structured_events_subscription: None,
            _reload_subscription: reload,
        };
        viewer.show(&item, window, cx);
        viewer
    }

    pub(crate) fn result(&self, cx: &App) -> Arc<ResultSet> {
        self.results_file.read(cx).result.clone()
    }

    /// Shows what the file holds now: a grid, or the reason there is none.
    fn show(&mut self, item: &Entity<ResultsFile>, window: &mut Window, cx: &mut Context<Self>) {
        let result = item.read(cx).result.clone();
        // A new file may not hold the focused activity, so a reload shows the whole trace.
        self.focus = None;
        self.focus_input = None;
        self.focus_note = None;
        self._focus_task = None;
        self.problem = item.read(cx).problem;
        self.grid = (self.problem.is_none() && !result.tables.is_empty())
            .then(|| cx.new(|cx| ResultGrid::new(result.clone(), 0, window, cx)));
        self._grid_subscription = self
            .grid
            .as_ref()
            .map(|grid| Self::watch_grid(grid, window, cx));
        if self.hide_structural {
            if let Some(grid) = &self.grid {
                grid.update(cx, |grid, cx| grid.set_hide_structural(true, cx));
            }
        }
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .filter(|_| self.grid.is_some())
            .map(|table| settings.trace_columns(table));
        self.can_show_structured = columns.is_some_and(|columns| columns.supports_activity());
        self.can_show_sequence = columns.is_some_and(|columns| columns.supports_sequence());
        self.can_show_timeline = columns
            .is_some_and(|columns| columns.supports_activity() && columns.timestamp.is_some());
        self.timeline_missing = columns
            .filter(|columns| columns.supports_activity() && columns.timestamp.is_none())
            .map(|_| SharedString::from("Timeline needs: timestamp"));
        self.sequence_missing = columns
            .filter(|columns| columns.supports_activity() && !columns.supports_sequence())
            .map(|columns| {
                let roles: Vec<&str> = columns
                    .missing_for_sequence()
                    .into_iter()
                    .map(TraceRole::label)
                    .collect();
                SharedString::from(format!("Sequence needs: {}", roles.join(", ")))
            });
        self.sequence_options = settings.sequence.clone();
        self.findings = None;
        self._findings_subscription = None;
        if self.can_show_structured {
            self.build_findings(window, cx);
        }
        // What the structured and sequence views were built from has changed.
        self.structured = None;
        self._structured_subscription = None;
        self._structured_events_subscription = None;
        self.sequence = None;
        self.timeline = None;
        self._timeline_subscription = None;
        self.can_show_query = self.grid.is_some()
            && result
                .query
                .as_deref()
                .is_some_and(|query| !query.trim().is_empty());
        self.query_view = None;
        if self.mode == ViewMode::Structured && !self.can_show_structured
            || self.mode == ViewMode::Sequence && !self.can_show_sequence
            || self.mode == ViewMode::Timeline && !self.can_show_timeline
            || self.mode == ViewMode::Query && !self.can_show_query
        {
            self.mode = ViewMode::Data;
        } else if self.mode == ViewMode::Structured {
            self.build_structured(window, cx);
        } else if self.mode == ViewMode::Sequence {
            self.build_sequence(window, cx);
        } else if self.mode == ViewMode::Timeline {
            self.build_timeline(window, cx);
        } else if self.mode == ViewMode::Query {
            self.build_query_view(window, cx);
        }
    }

    fn build_query_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        self.query_view = Some(cx.new(|cx| QueryView::new(&result, window, cx)));
    }

    /// Saves a copy of this result's file into the project, under a name made from its query.
    pub(crate) fn save_action(&self, cx: &App) -> Option<crate::save_result::SaveResult> {
        let file = self.results_file.read(cx);
        let path = file
            ._worktree
            .read(cx)
            .absolutize(&file.project_path.path)
            .to_string_lossy()
            .into_owned();
        let query = file.result.query.as_deref().unwrap_or_default();
        Some(crate::save_result::SaveResult {
            path: Some(path),
            suggested_name: Some(crate::save_result::suggested_local_file_name(
                query,
                file.result.execution_started_at.as_deref(),
            )),
        })
    }

    /// Runs the query of this result again, if the file says where it ran, with the values its
    /// parameters had.
    pub(crate) fn rerun_action(&self, cx: &App) -> Option<RerunQuery> {
        let result = &self.results_file.read(cx).result;
        let text = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        Some(RerunQuery {
            query: result
                .query
                .as_deref()
                .filter(|query| !query.trim().is_empty())?
                .to_string(),
            cluster: text(&result.cluster)?,
            database: text(&result.database)?,
            parameters: parameter_values(result).into_iter().collect(),
        })
    }

    /// Writes a grid's column layout back to the file, and does what its context menu asks.
    fn watch_grid(
        grid: &Entity<ResultGrid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(grid, window, |this, _, event: &ResultGridEvent, window, cx| match event {
            ResultGridEvent::LayoutChanged(layout) => {
                this.results_file
                    .update(cx, |file, cx| file.save_layout(layout.clone(), cx));
            }
            ResultGridEvent::FocusRequested { source_row } => {
                this.request_focus(FocusRequest::Row(*source_row), window, cx)
            }
            ResultGridEvent::SelectionChanged { .. } => {}
        })
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn structured_view(&self) -> Option<&Entity<StructuredView>> {
        match &self.structured {
            Some(Structured::Ready(view)) => Some(view),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn findings_strip(&self) -> Option<&Entity<FindingsStrip>> {
        match &self.findings {
            Some(FindingsState::Ready(strip)) => Some(strip),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn grid(&self) -> Option<&Entity<ResultGrid>> {
        self.grid.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn sequence_view(&self) -> Option<&Entity<SequenceView>> {
        match &self.sequence {
            Some(Sequence::Ready(view)) => Some(view),
            _ => None,
        }
    }

    fn set_mode(&mut self, mode: ViewMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == ViewMode::Structured && !self.can_show_structured
            || mode == ViewMode::Sequence && !self.can_show_sequence
            || mode == ViewMode::Timeline && !self.can_show_timeline
            || mode == ViewMode::Query && !self.can_show_query
        {
            return;
        }
        self.mode = mode;
        if mode == ViewMode::Structured && self.structured.is_none() {
            self.build_structured(window, cx);
        }
        if mode == ViewMode::Sequence && self.sequence.is_none() {
            self.build_sequence(window, cx);
        }
        if mode == ViewMode::Timeline && self.timeline.is_none() {
            self.build_timeline(window, cx);
        }
        if mode == ViewMode::Query && self.query_view.is_none() {
            self.build_query_view(window, cx);
        }
        cx.notify();
    }

    /// Focuses every view on an activity and what is below it, found away from the window.
    fn request_focus(&mut self, request: FocusRequest, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .map(|table| settings.trace_columns(table));
        let asked = match &request {
            FocusRequest::Id(id) => id.trim().to_string(),
            FocusRequest::Row(row) => format!("row {}", row + 1),
        };
        self.focus_note = None;
        self._focus_task = Some(cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let table = result.tables.first()?;
                    let target = match &request {
                        FocusRequest::Id(id) => FocusTarget::Id(id),
                        FocusRequest::Row(row) => FocusTarget::Row(*row),
                    };
                    resolve_focus(table, &columns?, target)
                })
                .await;
            this.update_in(cx, |this, window, cx| match found {
                Some(focus) => {
                    this.focus_input = None;
                    this.apply_focus(Some(Arc::new(focus)), window, cx);
                }
                None => {
                    this.focus_note = Some(
                        if asked.starts_with("row ") {
                            format!("{asked} has no activity id to focus on.")
                        } else {
                            format!("No activity has the id {asked}.")
                        }
                        .into(),
                    );
                    cx.notify();
                }
            })
            .log_err();
        }));
    }

    /// Shows the whole trace with `None`, or one activity and what is below it, in every view.
    fn apply_focus(&mut self, focus: Option<Arc<Focus>>, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = focus;
        self.focus_note = None;
        self.structured = None;
        self._structured_subscription = None;
        self._structured_events_subscription = None;
        self.sequence = None;
        self.timeline = None;
        self._timeline_subscription = None;
        self.pending_structured_reveal = None;
        let rows = self.focus.as_ref().map(|focus| Arc::new(focus.rows.clone()));
        if let Some(grid) = &self.grid {
            grid.update(cx, |grid, cx| grid.set_scope(rows, cx));
        }
        self.findings = None;
        self._findings_subscription = None;
        if self.can_show_structured {
            self.build_findings(window, cx);
        }
        match self.mode {
            ViewMode::Structured => self.build_structured(window, cx),
            ViewMode::Sequence => self.build_sequence(window, cx),
            ViewMode::Timeline => self.build_timeline(window, cx),
            ViewMode::Data | ViewMode::Query => {}
        }
        cx.notify();
    }

    fn focus_up_one_level(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.focus.as_ref().and_then(|focus| focus.parent_id.clone()) else {
            return;
        };
        self.request_focus(FocusRequest::Id(parent), window, cx);
    }

    /// Opens or closes the field an activity id is typed into.
    fn toggle_focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_input.take().is_some() {
            self.focus_note = None;
            cx.notify();
            return;
        }
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Activity id", window, cx);
            editor
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.focus_input = Some(FocusInput { editor });
        cx.notify();
    }

    fn confirm_focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = &self.focus_input else {
            return;
        };
        let text = input.editor.read(cx).text(cx);
        if text.trim().is_empty() {
            return;
        }
        self.request_focus(FocusRequest::Id(text), window, cx);
    }

    /// The bar above the tabs: what is focused, and the field a Focus… id is typed in.
    fn render_focus_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.focus.is_none() && self.focus_input.is_none() {
            return None;
        }
        let colors = cx.theme().colors();
        let focused = self.focus.as_ref().map(|focus| {
            let name = focus
                .marker
                .as_deref()
                .map(kusto_results::trace_text::short_marker)
                .unwrap_or_default();
            (name, focus.activity_id.clone(), focus.parent_id.is_some())
        });
        Some(
            v_flex()
                .key_context("KustoFocus")
                .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                    this.confirm_focus_input(window, cx)
                }))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                    if this.focus_input.is_some() {
                        this.toggle_focus_input(window, cx);
                    }
                }))
                .border_b_1()
                .border_color(colors.border)
                .bg(colors.surface_background)
                .when_some(focused, |bar, (name, id, has_parent)| {
                    bar.child(
                        h_flex()
                            .debug_selector(|| "focus-bar".to_string())
                            .px_2()
                            .py_1()
                            .gap_2()
                            .items_center()
                            .child(Label::new("Focused on").size(LabelSize::Small).color(Color::Muted))
                            .child(Label::new(name).size(LabelSize::Small).weight(gpui::FontWeight::SEMIBOLD))
                            .child(Label::new(id).size(LabelSize::Small).color(Color::Muted))
                            .child(div().flex_1())
                            .child(
                                div().debug_selector(|| "focus-up".to_string()).child(
                                    Button::new("results-focus-up", "Up one level")
                                        .size(ButtonSize::Compact)
                                        .disabled(!has_parent)
                                        .tooltip(ui::Tooltip::text("Focus on the parent of this activity"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.focus_up_one_level(window, cx)
                                        })),
                                ),
                            )
                            .child(
                                div().debug_selector(|| "focus-clear".to_string()).child(
                                    Button::new("results-focus-clear", "Show whole trace")
                                        .size(ButtonSize::Compact)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_focus(None, window, cx)
                                        })),
                                ),
                            ),
                    )
                })
                .when_some(self.focus_input.as_ref(), |bar, input| {
                    bar.child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_2()
                            .items_center()
                            .child(Label::new("Focus on activity").size(LabelSize::Small).color(Color::Muted))
                            .child(
                                div()
                                    .debug_selector(|| "focus-input".to_string())
                                    .flex_1()
                                    .px_2()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(colors.border)
                                    .child(input.editor.clone()),
                            )
                            .child(
                                div().debug_selector(|| "focus-confirm".to_string()).child(
                                    Button::new("results-focus-confirm", "Focus")
                                        .size(ButtonSize::Compact)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_focus_input(window, cx)
                                        })),
                                ),
                            )
                            .child(
                                Button::new("results-focus-cancel", "Cancel")
                                    .size(ButtonSize::Compact)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_focus_input(window, cx)
                                    })),
                            ),
                    )
                })
                .children(self.focus_note.clone().map(|note| {
                    div().px_3().pb_1().child(
                        Label::new(note).size(LabelSize::XSmall).color(Color::Warning),
                    )
                }))
                .into_any_element(),
        )
    }

    /// Builds the timeline away from the window, then the view.
    fn build_timeline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        let focus = self.focus.clone();
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .map(|table| settings.trace_columns(table));
        let options = settings.waterfall.clone();
        self.timeline = Some(TimelineState::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let for_build = result.clone();
                let built = cx
                    .background_spawn(async move {
                        let table = for_build.tables.first()?;
                        let columns = columns?;
                        let projection = build_projection_in(table, &columns, focus.as_deref())?;
                        let waterfall = Waterfall::build(table, &projection, &columns, &options)?;
                        Some((projection, waterfall))
                    })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let Some((projection, waterfall)) = built else {
                        this.timeline = None;
                        this.can_show_timeline = false;
                        this.mode = ViewMode::Data;
                        cx.notify();
                        return;
                    };
                    let view = cx.new(|_| {
                        WaterfallView::new(result, Arc::new(projection), Arc::new(waterfall))
                    });
                    this._timeline_subscription = Some(cx.subscribe_in(
                        &view,
                        window,
                        |this, _, event: &WaterfallEvent, window, cx| {
                            match event {
                                WaterfallEvent::OpenActivity(activity) => {
                                    this.open_in_structured(*activity, window, cx)
                                }
                                WaterfallEvent::FocusRequested(id) => {
                                    this.request_focus(FocusRequest::Id(id.clone()), window, cx)
                                }
                            }
                        },
                    ));
                    this.timeline = Some(TimelineState::Ready(view));
                    cx.notify();
                })
                .log_err();
            }),
        });
    }

    /// Shows an activity in the Structured tab, opening the branches above it.
    fn open_in_structured(&mut self, activity: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(ViewMode::Structured, window, cx);
        if self.mode != ViewMode::Structured {
            return;
        }
        match self.structured_view().cloned() {
            Some(view) => {
                let tree = view.read(cx).tree().clone();
                tree.update(cx, |tree, cx| tree.reveal(activity, cx));
            }
            None => self.pending_structured_reveal = Some(activity),
        }
    }

    #[cfg(test)]
    pub(crate) fn waterfall_view(&self) -> Option<&Entity<WaterfallView>> {
        match &self.timeline {
            Some(TimelineState::Ready(view)) => Some(view),
            _ => None,
        }
    }

    /// Works out the findings of a trace away from the window, then shows the strip.
    fn build_findings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        let focus = self.focus.clone();
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .map(|table| settings.trace_columns(table));
        let options = FindingsOptions {
            generic_actor_suffixes: settings.sequence.generic_actor_suffixes.clone(),
            timeline: TimelineOptions {
                structural_messages: settings.structural_messages.clone(),
                ..TimelineOptions::default()
            },
            ..FindingsOptions::default()
        };
        self.findings = Some(FindingsState::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let findings = cx
                    .background_spawn(async move {
                        let table = result.tables.first()?;
                        let columns = columns?;
                        let projection = build_projection_in(table, &columns, focus.as_deref())?;
                        Some(Findings::build(table, &projection, &columns, &options))
                    })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let Some(findings) = findings.filter(|findings| !findings.items.is_empty())
                    else {
                        this.findings = None;
                        cx.notify();
                        return;
                    };
                    let strip = cx.new(|_| FindingsStrip::new(Arc::new(findings)));
                    this._findings_subscription = Some(cx.subscribe_in(
                        &strip,
                        window,
                        |this, strip, event: &FindingsStripEvent, window, cx| {
                            let FindingsStripEvent::Chosen(index) = event;
                            this.choose_finding(strip, *index, window, cx);
                        },
                    ));
                    this.findings = Some(FindingsState::Ready(strip));
                    cx.notify();
                })
                .log_err();
            }),
        });
    }

    /// Shows what a finding points at: in the Structured tab the activity, otherwise its rows in
    /// the Data tab.
    fn choose_finding(
        &mut self,
        strip: &Entity<FindingsStrip>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(finding) = strip.read(cx).finding(index).cloned() else {
            return;
        };
        let note = self.show_finding(&finding, window, cx);
        strip.update(cx, |strip, cx| strip.set_note(note, cx));
    }

    fn show_finding(
        &mut self,
        finding: &Finding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<SharedString> {
        if self.mode == ViewMode::Structured {
            if let (Some(activity), Some(view)) =
                (finding.activities.first().copied(), self.structured_view().cloned())
            {
                let tree = view.read(cx).tree().clone();
                tree.update(cx, |tree, cx| tree.reveal(activity, cx));
                return None;
            }
        }
        if self.mode == ViewMode::Timeline {
            if let (Some(activity), Some(TimelineState::Ready(view))) =
                (finding.activities.first().copied(), &self.timeline)
            {
                let view = view.clone();
                view.update(cx, |view, cx| view.reveal(activity, cx));
                return None;
            }
        }
        if finding.rows.is_empty() {
            return Some("This finding has no rows to select.".into());
        }
        self.set_mode(ViewMode::Data, window, cx);
        let grid = self.grid.clone()?;
        let shown = grid.update(cx, |grid, cx| {
            grid.select_source_rows(&finding.rows, window, cx)
        });
        match shown {
            0 => Some("Those rows are hidden by the filters or the search.".into()),
            shown if shown < finding.rows.len() => Some(
                format!(
                    "{shown} of {} rows are shown; the rest are hidden by the filters or the search.",
                    finding.rows.len()
                )
                .into(),
            ),
            _ => None,
        }
    }

    fn toggle_hide_structural(&mut self, cx: &mut Context<Self>) {
        self.hide_structural = !self.hide_structural;
        let hide = self.hide_structural;
        let structured = self.structured_view().map(|view| view.read(cx).grid().clone());
        for grid in self.grid.iter().cloned().chain(structured) {
            grid.update(cx, |grid, cx| grid.set_hide_structural(hide, cx));
        }
        cx.notify();
    }

    /// Steps off, then 1, 2 and 3 levels below the root, then off again.
    fn cycle_step_depth(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sequence_options.step_depth = match self.sequence_options.step_depth {
            None => Some(1),
            Some(depth) if depth < 3 => Some(depth + 1),
            Some(_) => None,
        };
        self.draw_sequence_again(window, cx);
    }

    fn toggle_collapsed_loops(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sequence_options.collapse_repeats = !self.sequence_options.collapse_repeats;
        self.draw_sequence_again(window, cx);
    }

    fn draw_sequence_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.build_sequence(window, cx);
        cx.notify();
    }

    /// Builds the sequence diagram away from the window, then the view.
    fn build_sequence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        let focus = self.focus.clone();
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .map(|table| settings.trace_columns(table));
        let options = self.sequence_options.clone();
        self.sequence = Some(Sequence::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let mermaid = cx
                    .background_spawn(async move {
                        let table = result.tables.first()?;
                        let columns = columns?;
                        let projection = build_projection_in(table, &columns, focus.as_deref())?;
                        build_sequence(table, &projection, &columns, &options)
                            .map(|diagram| diagram.to_mermaid())
                    })
                    .await;
                this.update_in(cx, |this, _, cx| {
                    let Some(mermaid) = mermaid else {
                        this.sequence = None;
                        this.can_show_sequence = false;
                        this.mode = ViewMode::Data;
                        cx.notify();
                        return;
                    };
                    let view = cx.new(|cx| SequenceView::new(mermaid, cx));
                    this.sequence = Some(Sequence::Ready(view));
                    cx.notify();
                })
                .log_err();
            }),
        });
    }

    /// Builds the activity projection away from the window, then the view.
    fn build_structured(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        let focus = self.focus.clone();
        let settings = ResultsSettings::get_global(cx);
        let columns = result
            .tables
            .first()
            .map(|table| settings.trace_columns(table));
        let timeline_options = TimelineOptions {
            structural_messages: settings.structural_messages.clone(),
            ..TimelineOptions::default()
        };
        self.structured = Some(Structured::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let for_projection = result.clone();
                let built = cx
                    .background_spawn(async move {
                        let table = for_projection.tables.first()?;
                        let columns = columns?;
                        let projection = build_projection_in(table, &columns, focus.as_deref())?;
                        let timeline = columns.timestamp.is_some().then(|| {
                            Timeline::build(table, &projection, &columns, &timeline_options)
                        });
                        Some((projection, timeline))
                    })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let Some((projection, timeline)) = built else {
                        this.structured = None;
                        this.can_show_structured = false;
                        this.mode = ViewMode::Data;
                        cx.notify();
                        return;
                    };
                    let view = cx.new(|cx| {
                        StructuredView::new(
                            result,
                            0,
                            Arc::new(projection),
                            timeline.map(Arc::new),
                            window,
                            cx,
                        )
                    });
                    let grid = view.read(cx).grid().clone();
                    if this.hide_structural {
                        grid.update(cx, |grid, cx| grid.set_hide_structural(true, cx));
                    }
                    this._structured_subscription = Some(Self::watch_grid(&grid, window, cx));
                    this._structured_events_subscription = Some(cx.subscribe_in(
                        &view,
                        window,
                        |this, _, event: &StructuredViewEvent, window, cx| {
                            let StructuredViewEvent::FocusRequested(id) = event;
                            this.request_focus(FocusRequest::Id(id.clone()), window, cx);
                        },
                    ));
                    if let Some(activity) = this.pending_structured_reveal.take() {
                        let tree = view.read(cx).tree().clone();
                        tree.update(cx, |tree, cx| tree.reveal(activity, cx));
                    }
                    this.structured = Some(Structured::Ready(view));
                    cx.notify();
                })
                .log_err();
            }),
        });
    }
}

impl Render for ResultsViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match (self.mode, &self.timeline) {
            (ViewMode::Timeline, Some(TimelineState::Ready(view))) => {
                div().size_full().child(view.clone())
            }
            (ViewMode::Timeline, _) => div()
                .p_4()
                .child(ui::Label::new("Drawing the timeline…")),
            _ => match (
            self.mode,
            &self.structured,
            &self.sequence,
            &self.grid,
            &self.query_view,
        ) {
            (ViewMode::Query, _, _, _, Some(view)) => div().size_full().child(view.clone()),
            (ViewMode::Structured, Some(Structured::Ready(view)), _, _, _) => {
                div().size_full().child(view.clone())
            }
            (ViewMode::Structured, _, _, _, _) => div()
                .p_4()
                .child(ui::Label::new("Building the activity tree…")),
            (ViewMode::Sequence, _, Some(Sequence::Ready(view)), _, _) => {
                div().size_full().child(view.clone())
            }
            (ViewMode::Sequence, _, _, _, _) => div()
                .p_4()
                .child(ui::Label::new("Drawing the sequence diagram…")),
            (_, _, _, Some(grid), _) => div().size_full().child(grid.clone()),
            (_, _, _, None, _) => div()
                .p_4()
                .child(ui::Label::new(self.problem.unwrap_or(NO_RESULT_DATA))),
            },
        };
        let focus_bar = self.render_focus_bar(cx);
        let can_focus = self.can_show_structured && self.grid.is_some();
        let focus_open = self.focus_input.is_some();
        let structural_rows = self
            .grid
            .as_ref()
            .map_or(0, |grid| grid.read(cx).structural_row_count());
        let hide_structural = self.hide_structural;
        let strip = match &self.findings {
            Some(FindingsState::Ready(strip)) if self.grid.is_some() => Some(strip.clone()),
            _ => None,
        };
        let mode = self.mode;
        let steps = self.sequence_options.step_depth;
        let collapse = self.sequence_options.collapse_repeats;
        let rerun = self.rerun_action(cx);
        let save = self.save_action(cx);
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .when(self.grid.is_some(), |viewer| {
                viewer.child(
                    h_flex()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            div().debug_selector(|| "data-tab".to_string()).child(
                                Button::new("results-data-tab", "Data")
                                    .toggle_state(mode == ViewMode::Data)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_mode(ViewMode::Data, window, cx)
                                    })),
                            ),
                        )
                        .when(self.can_show_structured, |bar| {
                            bar.child(
                                div().debug_selector(|| "structured-tab".to_string()).child(
                                    Button::new("results-structured-tab", "Structured")
                                        .toggle_state(mode == ViewMode::Structured)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_mode(ViewMode::Structured, window, cx)
                                        })),
                                ),
                            )
                        })
                        .when(self.can_show_sequence, |bar| {
                            bar.child(
                                div().debug_selector(|| "sequence-tab".to_string()).child(
                                    Button::new("results-sequence-tab", "Sequence")
                                        .toggle_state(mode == ViewMode::Sequence)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_mode(ViewMode::Sequence, window, cx)
                                        })),
                                ),
                            )
                        })
                        .when(mode == ViewMode::Sequence, |bar| {
                            let steps = match steps {
                                Some(depth) => depth.to_string(),
                                None => "off".to_string(),
                            };
                            bar.child(
                                div().debug_selector(|| "sequence-steps".to_string()).child(
                                    Button::new("results-sequence-steps", format!("Steps: {steps}"))
                                        .tooltip(ui::Tooltip::text(
                                            "Group the calls under the activity this many levels below the root",
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.cycle_step_depth(window, cx)
                                        })),
                                ),
                            )
                            .child(
                                div().debug_selector(|| "sequence-collapse".to_string()).child(
                                    Button::new("results-sequence-collapse", "Collapse loops")
                                        .toggle_state(collapse)
                                        .tooltip(ui::Tooltip::text(
                                            "Draw calls that repeat as one loop with a count",
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_collapsed_loops(window, cx)
                                        })),
                                ),
                            )
                        })
                        .when(self.can_show_timeline, |bar| {
                            bar.child(
                                div().debug_selector(|| "timeline-tab".to_string()).child(
                                    Button::new("results-timeline-tab", "Timeline")
                                        .toggle_state(mode == ViewMode::Timeline)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_mode(ViewMode::Timeline, window, cx)
                                        })),
                                ),
                            )
                        })
                        .when_some(self.timeline_missing.clone(), |bar, missing| {
                            bar.child(
                                Label::new(missing)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        })
                        .when_some(self.sequence_missing.clone(), |bar, missing| {
                            bar.child(
                                Label::new(missing)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        })
                        .when(self.can_show_query, |bar| {
                            bar.child(
                                div().debug_selector(|| "query-tab".to_string()).child(
                                    Button::new("results-query-tab", "Query")
                                        .toggle_state(mode == ViewMode::Query)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.set_mode(ViewMode::Query, window, cx)
                                        })),
                                ),
                            )
                        })
                        .child(div().flex_1())
                        .when(can_focus, |bar| {
                            bar.child(
                                div().debug_selector(|| "focus-button".to_string()).child(
                                    Button::new("results-focus", "Focus…")
                                        .toggle_state(focus_open)
                                        .tooltip(ui::Tooltip::text(
                                            "Rebuild every view from one activity, by its id, and what is below it",
                                        ))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_focus_input(window, cx)
                                        })),
                                ),
                            )
                        })
                        .when(
                            structural_rows > 0 && matches!(mode, ViewMode::Data | ViewMode::Structured),
                            |bar| {
                                bar.child(
                                    div().debug_selector(|| "hide-structural".to_string()).child(
                                        Button::new("results-hide-structural", "Hide structural rows")
                                            .toggle_state(hide_structural)
                                            .tooltip(ui::Tooltip::text(format!(
                                                "{structural_rows} rows only say something started or ended. They are dimmed either way; this only hides them from the view"
                                            )))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_hide_structural(cx)
                                            })),
                                    ),
                                )
                            },
                        )
                        .when_some(save, |bar, action| {
                            bar.child(
                                div().debug_selector(|| "save-button".to_string()).child(
                                    Button::new("results-save", "Save a copy…")
                                        .tooltip(ui::Tooltip::text(
                                            "Save a copy of this result in your project to keep it",
                                        ))
                                        .on_click(move |_, window, cx| {
                                            window.dispatch_action(action.boxed_clone(), cx)
                                        }),
                                ),
                            )
                        })
                        .when_some(rerun, |bar, action| {
                            bar.child(
                                div().debug_selector(|| "rerun-button".to_string()).child(
                                    Button::new("results-rerun", "Run again")
                                        .start_icon(
                                            Icon::new(IconName::PlayFilled).size(IconSize::Small),
                                        )
                                        .tooltip(ui::Tooltip::text(
                                            "Run this query again, with the values it had",
                                        ))
                                        .on_click(move |_, window, cx| {
                                            window.dispatch_action(action.boxed_clone(), cx)
                                        }),
                                ),
                            )
                        }),
                )
            })
            .children(focus_bar)
            .children(strip)
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use fs::{FakeFs, Fs as _};
    use gpui::TestAppContext;
    use kusto_results::TableView;
    use project::Project;
    use serde_json::json;
    use workspace::AppState;

    use super::*;

    fn sample(name: &str) -> anyhow::Result<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fork-docs/samples")
            .join(name);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    #[test]
    fn routes_only_results_files_to_the_viewer() -> anyhow::Result<()> {
        for (name, expected) in [
            ("a.ktt", true),
            ("a.kqr", true),
            ("a.ktt.json", false),
            ("a.txt", false),
            ("a.kttx", false),
        ] {
            let path = RelPath::new(Path::new(name), util::paths::PathStyle::Unix)?;
            assert_eq!(is_results_path(&path), expected, "{name}");
        }
        Ok(())
    }

    #[gpui::test]
    async fn opens_fixtures_in_the_results_viewer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({
                "types.ktt": sample("synthetic-types.ktt").expect("fixture"),
                "legacy.kqr": sample("synthetic-legacy.kqr").expect("fixture"),
                "broken.ktt": "not json",
                "no-tables.ktt": "{\"query\": \"q\"}",
                "empty-tables.ktt": "{\"tables\": []}",
            }),
        )
        .await;
        let project = Project::test(fs, ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project.read_with(cx, |project, cx| {
            project
                .worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
        });
        let worktree_id = worktree_id.expect("the test project has a worktree");

        for name in ["types.ktt", "legacy.kqr"] {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path(path, None, true, window, cx)
                })
                .await
                .expect(name);
            let viewer = item
                .downcast::<ResultsViewer>()
                .unwrap_or_else(|| panic!("{name} opens in the results viewer"));
            let grid = viewer.read_with(cx, |viewer, _| viewer.grid.clone());
            let grid = grid.unwrap_or_else(|| panic!("{name} has a grid"));
            assert!(
                grid.read_with(cx, |grid, _| grid.visible_row_count()) > 0,
                "{name}"
            );
        }

        for (name, message) in [
            ("broken.ktt", "Invalid result file."),
            ("no-tables.ktt", "No result data found."),
            ("empty-tables.ktt", "No result data found."),
        ] {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path(path, None, true, window, cx)
                })
                .await
                .expect(name);
            let viewer = item
                .downcast::<ResultsViewer>()
                .unwrap_or_else(|| panic!("{name} opens in the results viewer"));
            assert!(
                viewer.read_with(cx, |viewer, _| viewer.grid.is_none()),
                "{name}"
            );
            assert_eq!(
                viewer.read_with(cx, |viewer, _| viewer.problem),
                Some(message),
                "{name}"
            );
        }
    }

    /// PER-4: a layout change made in an open result is written back, and nothing else in the
    /// file changes.
    #[gpui::test]
    async fn layout_changes_are_written_back_to_the_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let original = sample("synthetic-types.ktt").expect("fixture");
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "types.ktt": original }))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let path = ProjectPath {
            worktree_id,
            path: RelPath::new(Path::new("types.ktt"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let item = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("the file opens");
        let viewer = item.downcast::<ResultsViewer>().expect("a results viewer");
        let grid = viewer
            .read_with(cx, |viewer, _| viewer.grid.clone())
            .expect("a grid");

        let layout = TableView {
            name: grid.read_with(cx, |grid, _| grid.table_name()),
            gutter_width: Some(70),
            columns: Some(vec![kusto_results::ColumnLayout {
                index: 0,
                width: Some(123),
            }]),
        };
        grid.update(cx, |_, cx| {
            cx.emit(ResultGridEvent::LayoutChanged(layout.clone()))
        });
        let grid_id = grid.entity_id();
        cx.executor().advance_clock(SAVE_DELAY * 2);
        cx.run_until_parked();

        let still_shown = viewer.read_with(cx, |viewer, _| {
            viewer.grid.as_ref().map(|grid| grid.entity_id())
        });
        assert_eq!(
            still_shown,
            Some(grid_id),
            "the viewer's own write does not rebuild the grid"
        );
        let written = fs
            .load(Path::new("/root/types.ktt"))
            .await
            .expect("the file can be read");
        let reread = ResultSet::from_json(&written).expect("the written file parses");
        assert_eq!(reread.table_view(&layout.name), Some(&layout));
        let mut expected = ResultSet::from_json(&sample("synthetic-types.ktt").expect("fixture"))
            .expect("the fixture parses");
        expected.set_table_view(layout);
        assert_eq!(reread, expected, "only the saved layout changed");
    }

    /// PER-4: an edit made outside Zed shows in the open tab.
    #[gpui::test]
    async fn an_outside_edit_reloads_the_open_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({ "types.ktt": sample("synthetic-types.ktt").expect("fixture") }),
        )
        .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let path = ProjectPath {
            worktree_id,
            path: RelPath::new(Path::new("types.ktt"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let item = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("the file opens");
        let viewer = item.downcast::<ResultsViewer>().expect("a results viewer");
        let rows_shown = |cx: &mut gpui::VisualTestContext| {
            viewer.read_with(cx, |viewer, cx| {
                viewer
                    .grid
                    .as_ref()
                    .map(|grid| grid.read(cx).visible_row_count())
            })
        };
        let before = rows_shown(cx).expect("a grid");

        let other = sample("synthetic-trace-edge.ktt").expect("fixture");
        let other_rows = ResultSet::from_json(&other).expect("parses").tables[0]
            .rows
            .len();
        assert_ne!(before, other_rows);
        fs.insert_file("/root/types.ktt", other.into_bytes()).await;
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(rows_shown(cx), Some(other_rows), "the new content shows");

        fs.insert_file("/root/types.ktt", b"not json".to_vec())
            .await;
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(rows_shown(cx), None);
        assert_eq!(
            viewer.read_with(cx, |viewer, _| viewer.problem),
            Some("Invalid result file.")
        );
    }

    const MOVED: &str = r#"{
      "query": "// incident 123\ndeclare query_parameters(raid:string);\nT | where Id == raid",
      "cluster": "help.kusto.windows.net",
      "database": "Samples",
      "parameters": {"raid": "abc", "count": 5},
      "tables": [{"name": "PrimaryResult", "columns": [{"name": "n", "type": "long"}], "rows": [[1]]}],
      "executionStartedAt": "2026-10-01T10:00:00.000Z",
      "executionDurationMs": 1840
    }"#;

    const WITHOUT_QUERY: &str = r#"{
      "tables": [{"name": "PrimaryResult", "columns": [{"name": "n", "type": "long"}], "rows": [[1]]}]
    }"#;

    /// Opens each of `files` from a fresh project and returns the viewers, in order.
    async fn open_files<'a>(
        cx: &'a mut TestAppContext,
        files: &[(&'static str, &'static str)],
    ) -> (Vec<Entity<ResultsViewer>>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        let tree: serde_json::Map<String, serde_json::Value> = files
            .iter()
            .map(|(name, text)| (name.to_string(), json!(text)))
            .collect();
        fs.insert_tree("/root", serde_json::Value::Object(tree))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let mut viewers = Vec::new();
        for (name, _) in files {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path(path, None, true, window, cx)
                })
                .await
                .expect("opens");
            viewers.push(item.downcast::<ResultsViewer>().expect("a results viewer"));
            cx.run_until_parked();
        }
        (viewers, cx)
    }

    #[gpui::test]
    async fn a_result_file_shows_the_query_it_came_from_to_read_and_not_to_edit(
        cx: &mut TestAppContext,
    ) {
        let (viewers, cx) = open_files(cx, &[("moved.ktt", MOVED)]).await;
        let viewer = &viewers[0];
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("query-tab").is_some(),
            "the file names its query"
        );

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Query, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let (text, read_only) = viewer.read_with(cx, |viewer, cx| {
            let view = viewer.query_view.as_ref().expect("the query view is built");
            let editor = view.read(cx).editor().read(cx);
            (editor.text(cx), editor.read_only(cx))
        });
        assert!(
            text.starts_with("// incident 123\ndeclare query_parameters"),
            "{text}"
        );
        assert!(text.ends_with("T | where Id == raid"));
        assert!(read_only, "the query cannot be edited here");
        let details = viewer.read_with(cx, |viewer, cx| {
            let view = viewer.query_view.as_ref().expect("the query view");
            view.read(cx).parameters().to_vec()
        });
        assert_eq!(
            details,
            [
                ("count".to_string(), "5".to_string()),
                ("raid".to_string(), "abc".to_string())
            ]
        );
    }

    #[gpui::test]
    async fn a_result_file_can_be_run_again_with_the_values_it_had(cx: &mut TestAppContext) {
        let (viewers, cx) = open_files(cx, &[("moved.ktt", MOVED)]).await;
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("rerun-button").is_some());

        let action = viewers[0]
            .read_with(cx, |viewer, cx| viewer.rerun_action(cx))
            .expect("the file says where it ran");
        assert_eq!(action.cluster, "help.kusto.windows.net");
        assert_eq!(action.database, "Samples");
        assert!(action.query.ends_with("T | where Id == raid"));
        assert_eq!(
            action.parameters,
            std::collections::BTreeMap::from([
                ("count".to_string(), "5".to_string()),
                ("raid".to_string(), "abc".to_string()),
            ])
        );
    }

    #[gpui::test]
    async fn a_result_that_does_not_say_where_it_came_from_offers_neither(cx: &mut TestAppContext) {
        let (viewers, cx) = open_files(cx, &[("plain.ktt", WITHOUT_QUERY)]).await;
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("query-tab").is_none());
        assert!(cx.debug_bounds("rerun-button").is_none());
        assert!(
            viewers[0]
                .read_with(cx, |viewer, cx| viewer.rerun_action(cx))
                .is_none()
        );

        viewers[0].update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Query, window, cx)
        });
        assert_eq!(
            viewers[0].read_with(cx, |viewer, _| viewer.mode()),
            ViewMode::Data,
            "there is no query to show"
        );
    }

    /// ACT-6, PER-4: a trace offers a Structured tab next to Data, built when first asked for;
    /// a table without activity columns offers none.
    #[gpui::test]
    async fn a_trace_offers_a_structured_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({
                "trace.ktt": sample("synthetic-trace-edge.ktt").expect("fixture"),
                "types.ktt": sample("synthetic-types.ktt").expect("fixture"),
            }),
        )
        .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let plain = open("types.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(
            cx.debug_bounds("structured-tab").is_none(),
            "no activity columns, no tab"
        );
        assert!(!plain.read_with(cx, |viewer, _| viewer.can_show_structured));

        let trace = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(trace.read_with(cx, |viewer, _| viewer.can_show_structured));
        assert_eq!(
            trace.read_with(cx, |viewer, _| viewer.mode()),
            ViewMode::Data
        );
        assert!(trace.read_with(cx, |viewer, _| viewer.structured_view().is_none()));

        let tab = cx
            .debug_bounds("structured-tab")
            .map(|bounds| bounds.center())
            .expect("the tab shows");
        cx.simulate_click(tab, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            trace.read_with(cx, |viewer, _| viewer.mode()),
            ViewMode::Structured
        );
        let view = trace
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view is built");
        assert!(
            view.read_with(cx, |view, cx| view.grid().read(cx).visible_row_count()) > 0,
            "the first root's events show"
        );

        let data = cx
            .debug_bounds("data-tab")
            .map(|bounds| bounds.center())
            .expect("the tab shows");
        cx.simulate_click(data, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            trace.read_with(cx, |viewer, _| viewer.mode()),
            ViewMode::Data
        );
        assert!(
            trace.read_with(cx, |viewer, _| viewer.structured_view().is_some()),
            "switching back keeps the structured view"
        );
    }

    /// TRC-3, TRC-4: a trace whose columns are not named like the built-in ones offers the
    /// Structured tab once a schema in the settings names them.
    #[gpui::test]
    async fn a_trace_schema_in_the_settings_offers_the_structured_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let spans = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "SpanId", "type": "string" },
                    { "name": "ParentSpanId", "type": "string" },
                    { "name": "Message", "type": "string" },
                ],
                "rows": [["a", "", "start"], ["b", "a", "child"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "spans.ktt": spans, "again.ktt": spans }))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let without = open("spans.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(!without.read_with(cx, |viewer, _| viewer.can_show_structured));

        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store
                    .set_user_settings(
                        r#"{ "kusto_results": { "trace_schemas": [
                            { "name": "spans", "activity_id": "SpanId",
                              "parent_activity_id": "ParentSpanId" }
                        ] } }"#,
                        cx,
                    )
                    .expect("the user settings parse");
            });
        });
        let with = open("again.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(with.read_with(cx, |viewer, _| viewer.can_show_structured));

        with.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Structured, window, cx)
        });
        cx.run_until_parked();
        let view = with
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view is built from the schema");
        assert!(view.read_with(cx, |view, cx| view.grid().read(cx).visible_row_count()) > 0);
    }

    /// SEQ-1, SEQ-16: a trace with an actor and a timestamp offers a Sequence tab, drawn when first
    /// asked for; a trace without them says what is missing.
    #[gpui::test]
    async fn a_trace_offers_a_sequence_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let without_actor = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                ],
                "rows": [["a", ""], ["b", "a"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/root",
            json!({
                "trace.ktt": sample("synthetic-trace-edge.ktt").expect("fixture"),
                "types.ktt": sample("synthetic-types.ktt").expect("fixture"),
                "no-actor.ktt": without_actor,
            }),
        )
        .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let plain = open("types.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(!plain.read_with(cx, |viewer, _| viewer.can_show_sequence));
        assert!(plain.read_with(cx, |viewer, _| viewer.sequence_missing.is_none()));

        let partial = open("no-actor.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(!partial.read_with(cx, |viewer, _| viewer.can_show_sequence));
        assert_eq!(
            partial.read_with(cx, |viewer, _| viewer.sequence_missing.clone()),
            Some("Sequence needs: actor, timestamp".into())
        );

        let trace = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(trace.read_with(cx, |viewer, _| viewer.can_show_sequence));
        assert!(trace.read_with(cx, |viewer, _| viewer.sequence_view().is_none()));

        let tab = cx
            .debug_bounds("sequence-tab")
            .map(|bounds| bounds.center())
            .expect("the tab shows");
        cx.simulate_click(tab, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            trace.read_with(cx, |viewer, _| viewer.mode()),
            ViewMode::Sequence
        );
        let view = trace
            .read_with(cx, |viewer, _| viewer.sequence_view().cloned())
            .expect("the sequence view is built");
        let source = view.read_with(cx, |view, cx| view.source(cx));
        assert!(source.contains("sequenceDiagram"), "{source}");
    }

    /// SEQ-19: the step depth and loop collapsing can be changed from the tab, and the diagram is
    /// drawn again.
    #[gpui::test]
    async fn the_sequence_tab_draws_again_when_its_options_change(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = "2026-01-01T00:00:00.0000000Z";
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                    { "name": "ProcessName", "type": "string" },
                    { "name": "MarkerName", "type": "string" },
                    { "name": "TIMESTAMP", "type": "datetime" },
                ],
                "rows": [
                    ["r", "", "A", "Run", time],
                    ["s", "r", "A", "Phase", time],
                    ["c", "s", "A", "Ask", time],
                    ["k", "c", "B", "Entry", time],
                ],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace })).await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let path = ProjectPath {
            worktree_id,
            path: RelPath::new(Path::new("trace.ktt"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let viewer = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Sequence, window, cx)
        });
        cx.run_until_parked();

        let drawn_with_steps = |cx: &mut gpui::VisualTestContext| {
            let view = viewer
                .read_with(cx, |viewer, _| viewer.sequence_view().cloned())
                .expect("the sequence view is built");
            view.read_with(cx, |view, cx| view.source(cx).contains("rect"))
        };
        assert!(drawn_with_steps(cx), "one level below the root is the default");
        let mut seen = Vec::new();
        for _ in 0..3 {
            viewer.update_in(cx, |viewer, window, cx| viewer.cycle_step_depth(window, cx));
            cx.run_until_parked();
            seen.push(drawn_with_steps(cx));
        }
        assert_eq!(
            seen,
            vec![true, false, false],
            "two levels still groups under Ask, three has no ancestor that deep, and off draws none"
        );
        viewer.update_in(cx, |viewer, window, cx| viewer.cycle_step_depth(window, cx));
        cx.run_until_parked();
        assert!(drawn_with_steps(cx), "the cycle comes back to one level");

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.toggle_collapsed_loops(window, cx)
        });
        assert!(!viewer.read_with(cx, |viewer, _| viewer.sequence_options.collapse_repeats));
    }

    /// FND-1, FND-5: a trace shows its findings in a strip that opens, and choosing a finding
    /// selects its rows in the Data tab or its activity in the Structured tab.
    #[gpui::test]
    async fn a_trace_shows_findings_that_select_what_they_point_at(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = |millis: u32| format!("2026-01-01T00:00:00.{:07}Z", millis * 10_000);
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                    { "name": "ProcessName", "type": "string" },
                    { "name": "MarkerName", "type": "string" },
                    { "name": "TIMESTAMP", "type": "datetime" },
                    { "name": "Level", "type": "long" },
                    { "name": "MessageText", "type": "string" },
                ],
                "rows": [
                    ["r", "", "A", "Job.Run", time(0), 4, "start"],
                    ["a", "r", "A", "Ask", time(10), 4, "asking"],
                    ["b", "a", "B", "Db.Query", time(20), 2, "no such row"],
                    ["r", "", "A", "Job.Run", time(100), 4, "done"],
                ],
            }],
        })
        .to_string();
        let plain = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [{ "name": "Message", "type": "string" }],
                "rows": [["hello"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace, "plain.ktt": plain }))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let not_a_trace = open("plain.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(not_a_trace.read_with(cx, |viewer, _| viewer.findings_strip().is_none()));

        let viewer = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let strip = viewer
            .read_with(cx, |viewer, _| viewer.findings_strip().cloned())
            .expect("a trace shows its findings");
        assert!(strip.read_with(cx, |strip, _| strip.count()) >= 1);
        assert!(!strip.read_with(cx, |strip, _| strip.is_expanded()), "it starts closed");

        let header = cx
            .debug_bounds("findings-strip-header")
            .map(|bounds| bounds.center())
            .expect("the header shows");
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(strip.read_with(cx, |strip, _| strip.is_expanded()));

        let first = cx
            .debug_bounds("finding-0")
            .map(|bounds| bounds.center())
            .expect("the first finding shows");
        let failure_rows = strip
            .read_with(cx, |strip, _| strip.finding(0).map(|finding| finding.rows.clone()))
            .expect("a first finding");
        assert_eq!(failure_rows, vec![2], "the failure began on the row of the error");
        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Sequence, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(first, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.mode()), ViewMode::Data);
        let grid = viewer
            .read_with(cx, |viewer, _| viewer.grid().cloned())
            .expect("the grid");
        assert_eq!(
            grid.read_with(cx, |grid, _| grid.selected_source_rows()),
            vec![2],
            "the rows of the finding are selected"
        );
        assert!(strip.read_with(cx, |strip, _| strip.note().is_none()));

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Structured, window, cx)
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let first = cx
            .debug_bounds("finding-0")
            .map(|bounds| bounds.center())
            .expect("the first finding shows");
        cx.simulate_click(first, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.mode()), ViewMode::Structured);
        let structured = viewer
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view is built");
        assert_eq!(
            structured.read_with(cx, |view, cx| view.tree().read(cx).selected()),
            2,
            "the activity where the failure began is selected"
        );

        grid.update(cx, |grid, cx| grid.set_search("no match anywhere".into(), cx));
        cx.run_until_parked();
        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Data, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let first = cx
            .debug_bounds("finding-0")
            .map(|bounds| bounds.center())
            .expect("the first finding shows");
        cx.simulate_click(first, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            strip.read_with(cx, |strip, _| strip.note().cloned()),
            Some("Those rows are hidden by the filters or the search.".into())
        );
    }

    /// TPL-1: a result with rows that only say something started or ended offers to hide them,
    /// in the Data and Structured tabs.
    #[gpui::test]
    async fn the_viewer_offers_to_hide_structural_rows(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = "2026-01-01T00:00:00.0000000Z";
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                    { "name": "ProcessName", "type": "string" },
                    { "name": "TIMESTAMP", "type": "datetime" },
                    { "name": "MessageText", "type": "string" },
                ],
                "rows": [
                    ["r", "", "A", time, "Monitored scope start."],
                    ["r", "", "A", time, "Working"],
                    ["r", "", "A", time, "Monitored scope end."],
                ],
            }],
        })
        .to_string();
        let plain = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [{ "name": "MessageText", "type": "string" }],
                "rows": [["Working"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace, "plain.ktt": plain }))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let plain = open("plain.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hide-structural").is_none(), "no structural rows, no toggle");
        assert!(plain.read_with(cx, |viewer, _| viewer.grid().is_some()));

        let viewer = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let grid = viewer
            .read_with(cx, |viewer, _| viewer.grid().cloned())
            .expect("the grid");
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 3);

        let toggle = cx
            .debug_bounds("hide-structural")
            .map(|bounds| bounds.center())
            .expect("the toggle shows");
        cx.simulate_click(toggle, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_rows_for_test().to_vec()), vec![1]);

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Structured, window, cx)
        });
        cx.run_until_parked();
        let structured = viewer
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view is built");
        let structured_grid = structured.read_with(cx, |view, _| view.grid().clone());
        assert!(
            structured_grid.read_with(cx, |grid, _| grid.hides_structural_rows()),
            "a grid built after the choice keeps it"
        );

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Sequence, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("hide-structural").is_none(), "not in the sequence tab");

        viewer.update_in(cx, |viewer, window, cx| {
            viewer.set_mode(ViewMode::Data, window, cx)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let toggle = cx
            .debug_bounds("hide-structural")
            .map(|bounds| bounds.center())
            .expect("the toggle shows again");
        cx.simulate_click(toggle, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 3);
        assert!(!structured_grid.read_with(cx, |grid, _| grid.hides_structural_rows()));
    }

    /// WFL-1 to WFL-9: a trace with times offers a Timeline tab that folds short activities,
    /// selects an activity, links to the other tabs, and zooms.
    #[gpui::test]
    async fn the_timeline_tab_draws_a_trace_and_links_to_the_other_tabs(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = |millis: u32| {
            format!("2026-01-01T00:00:{:02}.{:07}Z", millis / 1000, (millis % 1000) * 10_000)
        };
        let columns = json!([
            { "name": "CurrentActivityId", "type": "string" },
            { "name": "ParentActivityId", "type": "string" },
            { "name": "ProcessName", "type": "string" },
            { "name": "MarkerName", "type": "string" },
            { "name": "TIMESTAMP", "type": "datetime" },
            { "name": "Level", "type": "long" },
            { "name": "MessageText", "type": "string" },
        ]);
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": columns,
                "rows": [
                    ["r", "", "A", "Run", time(0), 4, "start"],
                    ["r", "", "A", "Run", time(1000), 4, "end"],
                    ["big", "r", "B", "Big", time(100), 4, "start"],
                    ["big", "r", "B", "Big", time(900), 4, "end"],
                    ["tiny", "big", "B", "Tiny", time(150), 4, "start"],
                    ["tiny", "big", "B", "Tiny", time(155), 4, "end"],
                    ["bad", "r", "A", "Fail", time(950), 2, "it broke"],
                ],
            }],
        })
        .to_string();
        let untimed = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                ],
                "rows": [["r", ""], ["a", "r"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace, "untimed.ktt": untimed }))
            .await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let without = open("untimed.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        assert!(!without.read_with(cx, |viewer, _| viewer.can_show_timeline));
        assert_eq!(
            without.read_with(cx, |viewer, _| viewer.timeline_missing.clone()),
            Some("Timeline needs: timestamp".into())
        );

        let viewer = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(viewer.read_with(cx, |viewer, _| viewer.can_show_timeline));
        assert!(viewer.read_with(cx, |viewer, _| viewer.waterfall_view().is_none()), "built when asked for");

        let tab = cx
            .debug_bounds("timeline-tab")
            .map(|bounds| bounds.center())
            .expect("the tab shows");
        cx.simulate_click(tab, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.mode()), ViewMode::Timeline);
        let view = viewer
            .read_with(cx, |viewer, _| viewer.waterfall_view().cloned())
            .expect("the timeline is built");
        let rows = view.read_with(cx, |view, _| view.rows().to_vec());
        assert_eq!(rows.len(), 3, "Run, Big and the failure; Tiny is folded into Big");
        assert_eq!(rows.iter().map(|row| row.folded).collect::<Vec<_>>(), vec![0, 1, 0]);

        // Bars lie where their times put them: Run fills its row, and Big, which runs from 100 to
        // 900 ms of a 1,000 ms trace, covers the 10% to 90% of its row.
        let area = cx.debug_bounds("waterfall-bars-1").expect("the bars of Big's row");
        let run = cx.debug_bounds("waterfall-bar-0").expect("Run's bar");
        let big = cx.debug_bounds("waterfall-bar-1").expect("Big's bar");
        let near = |left: gpui::Pixels, right: gpui::Pixels| (f32::from(left) - f32::from(right)).abs() < 2.0;
        let width = area.size.width;
        assert!(near(run.size.width, width), "Run spans the whole axis: {:?} of {:?}", run.size.width, width);
        assert!(near(big.origin.x - area.origin.x, width * 0.1), "Big starts at 10%: {:?}", big.origin.x - area.origin.x);
        assert!(near(big.size.width, width * 0.8), "Big is 80% wide: {:?} of {:?}", big.size.width, width);

        let row = cx
            .debug_bounds("waterfall-row-1")
            .map(|bounds| bounds.center())
            .expect("a row shows");
        cx.simulate_click(row, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.selected()), Some(1));
        let followed = cx.update(|_, cx| {
            crate::row_details_panel::ActiveSelection::shared(cx).read(cx).rows.clone()
        });
        assert_eq!(followed, vec![2, 3], "the inspector follows the events of the activity");

        let show_all = cx
            .debug_bounds("waterfall-show-all")
            .map(|bounds| bounds.center())
            .expect("the button shows");
        cx.simulate_click(show_all, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.rows().len()), 4);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let show_all = cx
            .debug_bounds("waterfall-show-all")
            .map(|bounds| bounds.center())
            .expect("the button shows");
        cx.simulate_click(show_all, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.rows().len()), 3, "folded again");

        let (_, full_span) = view.read_with(cx, |view, _| view.view());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let zoom_in = cx
            .debug_bounds("waterfall-zoom-in")
            .map(|bounds| bounds.center())
            .expect("the button shows");
        cx.simulate_click(zoom_in, gpui::Modifiers::default());
        cx.run_until_parked();
        let (start, span) = view.read_with(cx, |view, _| view.view());
        assert!(span < full_span && start > 0, "zoomed in about the middle: {start} {span}");
        let fit = cx
            .debug_bounds("waterfall-fit")
            .map(|bounds| bounds.center())
            .expect("the button shows");
        cx.simulate_click(fit, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.view()), (0, full_span));

        // A finding chosen in this tab selects its activity here.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let header = cx
            .debug_bounds("findings-strip-header")
            .map(|bounds| bounds.center())
            .expect("the strip shows");
        cx.simulate_click(header, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let first = cx
            .debug_bounds("finding-0")
            .map(|bounds| bounds.center())
            .expect("the first finding shows");
        cx.simulate_click(first, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.mode()), ViewMode::Timeline);
        assert_eq!(view.read_with(cx, |view, _| view.selected()), Some(3), "the activity where the failure began");

        // A double-click on a row opens the activity in the Structured tab.
        view.update(cx, |_, cx| cx.emit(WaterfallEvent::OpenActivity(1)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |viewer, _| viewer.mode()), ViewMode::Structured);
        let structured = viewer
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view is built");
        assert_eq!(
            structured.read_with(cx, |view, cx| view.tree().read(cx).selected()),
            1,
            "the activity is revealed once the tree exists"
        );
    }

    /// FOC-1 to FOC-6: every view can be rebuilt from one activity, by its id, and go back.
    #[gpui::test]
    async fn every_view_can_be_focused_on_an_activity_and_back(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = |millis: u32| {
            format!("2026-01-01T00:00:{:02}.{:07}Z", millis / 1000, (millis % 1000) * 10_000)
        };
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                    { "name": "ProcessName", "type": "string" },
                    { "name": "MarkerName", "type": "string" },
                    { "name": "TIMESTAMP", "type": "datetime" },
                    { "name": "Level", "type": "long" },
                    { "name": "MessageText", "type": "string" },
                ],
                "rows": [
                    ["r", "", "A", "Run", time(0), 4, "start"],
                    ["a", "r", "B", "Step", time(100), 4, "start"],
                    ["c", "a", "B", "Inner", time(200), 4, "start"],
                    ["c", "a", "B", "Inner", time(300), 4, "end"],
                    ["a", "r", "B", "Step", time(600), 4, "end"],
                    ["b", "r", "A", "Other", time(700), 2, "it broke"],
                    ["r", "", "A", "Run", time(1000), 4, "end"],
                ],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace })).await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let path = ProjectPath {
            worktree_id,
            path: RelPath::new(Path::new("trace.ktt"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let viewer = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let grid = viewer
            .read_with(cx, |viewer, _| viewer.grid().cloned())
            .expect("the grid");
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 7);
        assert!(cx.debug_bounds("focus-bar").is_none(), "no focus, no bar");
        let strip = viewer
            .read_with(cx, |viewer, _| viewer.findings_strip().cloned())
            .expect("findings");
        assert_eq!(
            strip.read_with(cx, |strip, _| strip.finding(0).map(|finding| finding.title.clone())),
            Some("Failure began in A: Other".to_string())
        );

        // The field opens, and an unknown id says so and changes nothing.
        let button = cx
            .debug_bounds("focus-button")
            .map(|bounds| bounds.center())
            .expect("the Focus… button shows");
        cx.simulate_click(button, gpui::Modifiers::default());
        cx.run_until_parked();
        let editor = viewer
            .read_with(cx, |viewer, _| viewer.focus_input.as_ref().map(|input| input.editor.clone()))
            .expect("the field is open");
        editor.update_in(cx, |editor, window, cx| editor.set_text("nope", window, cx));
        viewer.update_in(cx, |viewer, window, cx| viewer.confirm_focus_input(window, cx));
        cx.run_until_parked();
        assert!(viewer.read_with(cx, |viewer, _| viewer.focus.is_none()));
        assert_eq!(
            viewer.read_with(cx, |viewer, _| viewer.focus_note.clone()),
            Some("No activity has the id nope.".into())
        );

        // An id, in any case, focuses every view on that activity and what is below it.
        editor.update_in(cx, |editor, window, cx| editor.set_text("  A ", window, cx));
        viewer.update_in(cx, |viewer, window, cx| viewer.confirm_focus_input(window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let focus = viewer
            .read_with(cx, |viewer, _| viewer.focus.clone())
            .expect("focused");
        assert_eq!((focus.activity_id.as_str(), focus.rows.clone()), ("a", vec![1, 2, 3, 4]));
        assert!(viewer.read_with(cx, |viewer, _| viewer.focus_input.is_none()), "the field closes");
        assert!(cx.debug_bounds("focus-bar").is_some(), "the bar shows");
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_rows_for_test().to_vec()), vec![1, 2, 3, 4]);
        let origins = viewer.read_with(cx, |viewer, cx| {
            viewer.findings_strip().map_or(0, |strip| {
                let strip = strip.read(cx);
                (0..strip.count())
                    .filter_map(|index| strip.finding(index))
                    .filter(|finding| finding.title.starts_with("Failure began"))
                    .count()
            })
        });
        assert_eq!(origins, 0, "the failure is outside the focus");

        viewer.update_in(cx, |viewer, window, cx| viewer.set_mode(ViewMode::Timeline, window, cx));
        cx.run_until_parked();
        let waterfall = viewer
            .read_with(cx, |viewer, _| viewer.waterfall_view().cloned())
            .expect("the timeline");
        let (_, extent) = waterfall.read_with(cx, |view, _| view.view());
        assert_eq!(extent, 500 * 10_000, "the axis is the focused step: 100 to 600 ms");

        viewer.update_in(cx, |viewer, window, cx| viewer.set_mode(ViewMode::Structured, window, cx));
        cx.run_until_parked();
        let structured = viewer
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view");
        let activities = structured.read_with(cx, |view, cx| {
            view.tree().read(cx).state().projection().activities.len()
        });
        assert_eq!(activities, 2, "a and c; r and b are outside");

        // Up one level, then the whole trace.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let up = cx
            .debug_bounds("focus-up")
            .map(|bounds| bounds.center())
            .expect("Up one level shows");
        cx.simulate_click(up, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let focus = viewer
            .read_with(cx, |viewer, _| viewer.focus.clone())
            .expect("focused");
        assert_eq!(focus.activity_id, "r");
        assert_eq!(focus.parent_id, None, "a root has no level above");
        assert_eq!(grid.read_with(cx, |grid, _| grid.visible_row_count()), 7);

        let clear = cx
            .debug_bounds("focus-clear")
            .map(|bounds| bounds.center())
            .expect("Show whole trace shows");
        cx.simulate_click(clear, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(viewer.read_with(cx, |viewer, _| viewer.focus.is_none()));
        assert!(cx.debug_bounds("focus-bar").is_none(), "the bar goes");
        let strip = viewer
            .read_with(cx, |viewer, _| viewer.findings_strip().cloned())
            .expect("findings again");
        assert_eq!(
            strip.read_with(cx, |strip, _| strip.finding(0).map(|finding| finding.title.clone())),
            Some("Failure began in A: Other".to_string()),
            "the whole trace has its failure back"
        );
    }

    /// FOC-1: a right-click on a grid row, a tree node or a timeline row offers to focus, and the
    /// request focuses every view.
    #[gpui::test]
    async fn the_context_menus_of_the_views_offer_to_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            AppState::test(cx);
            init(cx);
        });
        let time = |millis: u32| {
            format!("2026-01-01T00:00:{:02}.{:07}Z", millis / 1000, (millis % 1000) * 10_000)
        };
        let trace = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [
                    { "name": "CurrentActivityId", "type": "string" },
                    { "name": "ParentActivityId", "type": "string" },
                    { "name": "ProcessName", "type": "string" },
                    { "name": "MarkerName", "type": "string" },
                    { "name": "TIMESTAMP", "type": "datetime" },
                ],
                "rows": [
                    ["r", "", "A", "Run", time(0)],
                    ["a", "r", "B", "Step", time(100)],
                    ["c", "a", "B", "Inner", time(200)],
                    ["c", "a", "B", "Inner", time(300)],
                    ["a", "r", "B", "Step", time(600)],
                    ["r", "", "A", "Run", time(1000)],
                ],
            }],
        })
        .to_string();
        let plain = json!({
            "tables": [{
                "name": "PrimaryResult",
                "columns": [{ "name": "MessageText", "type": "string" }],
                "rows": [["hello"], ["world"]],
            }],
        })
        .to_string();
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/root", json!({ "trace.ktt": trace, "plain.ktt": plain })).await;
        let project = Project::test(fs.clone(), ["/root".as_ref()], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project.clone(), window, cx));
        cx.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(800.)));
        let worktree_id = project
            .read_with(cx, |project, cx| {
                project
                    .worktrees(cx)
                    .next()
                    .map(|worktree| worktree.read(cx).id())
            })
            .expect("the test project has a worktree");
        let open = |name: &'static str, cx: &mut gpui::VisualTestContext| {
            let path = ProjectPath {
                worktree_id,
                path: RelPath::new(Path::new(name), util::paths::PathStyle::Unix)
                    .expect("relative path")
                    .into_arc(),
            };
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
        };

        let viewer = open("trace.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));

        // The grid: a right click on a row opens its menu, and a request focuses on the row's activity.
        let grid = viewer
            .read_with(cx, |viewer, _| viewer.grid().cloned())
            .expect("the grid");
        let over_row = cx.debug_bounds("cell-2-1").expect("a cell").center();
        cx.simulate_mouse_down(over_row, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.simulate_mouse_up(over_row, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(grid.read_with(cx, |grid, _| grid.has_context_menu()));
        grid.update(cx, |_, cx| cx.emit(crate::grid::ResultGridEvent::FocusRequested { source_row: 2 }));
        cx.run_until_parked();
        let focus = viewer.read_with(cx, |viewer, _| viewer.focus.clone()).expect("focused");
        assert_eq!((focus.activity_id.as_str(), focus.rows.clone()), ("c", vec![2, 3]));
        viewer.update_in(cx, |viewer, window, cx| viewer.apply_focus(None, window, cx));
        cx.run_until_parked();
        grid.update(cx, |_, cx| cx.emit(crate::grid::ResultGridEvent::FocusRequested { source_row: 99 }));
        cx.run_until_parked();
        assert!(viewer.read_with(cx, |viewer, _| viewer.focus.is_none()));
        assert_eq!(
            viewer.read_with(cx, |viewer, _| viewer.focus_note.clone()),
            Some("row 100 has no activity id to focus on.".into())
        );

        // The tree: a right click on a node selects it and opens its menu; its request focuses.
        viewer.update_in(cx, |viewer, window, cx| viewer.set_mode(ViewMode::Structured, window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let structured = viewer
            .read_with(cx, |viewer, _| viewer.structured_view().cloned())
            .expect("the structured view");
        let tree = structured.read_with(cx, |view, _| view.tree().clone());
        let node = cx.debug_bounds("activity-node-0").expect("a node").center();
        cx.simulate_mouse_down(node, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.simulate_mouse_up(node, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(tree.read_with(cx, |tree, _| tree.has_context_menu()));
        tree.update(cx, |_, cx| {
            cx.emit(crate::activity_tree::ActivityTreeEvent::FocusRequested("a".to_string()))
        });
        cx.run_until_parked();
        assert_eq!(
            viewer.read_with(cx, |viewer, _| viewer.focus.clone().map(|focus| focus.activity_id.clone())),
            Some("a".to_string())
        );

        // The timeline: a right click on a row opens its menu; its request focuses.
        viewer.update_in(cx, |viewer, window, cx| viewer.apply_focus(None, window, cx));
        viewer.update_in(cx, |viewer, window, cx| viewer.set_mode(ViewMode::Timeline, window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let waterfall = viewer
            .read_with(cx, |viewer, _| viewer.waterfall_view().cloned())
            .expect("the timeline");
        let row = cx.debug_bounds("waterfall-row-1").expect("a row").center();
        cx.simulate_mouse_down(row, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.simulate_mouse_up(row, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(waterfall.read_with(cx, |view, _| view.has_context_menu()));
        waterfall.update(cx, |_, cx| cx.emit(WaterfallEvent::FocusRequested("c".to_string())));
        cx.run_until_parked();
        assert_eq!(
            viewer.read_with(cx, |viewer, _| viewer.focus.clone().map(|focus| focus.activity_id.clone())),
            Some("c".to_string())
        );

        // A table that is not a trace offers no focus.
        let plain = open("plain.ktt", cx)
            .await
            .expect("opens")
            .downcast::<ResultsViewer>()
            .expect("a results viewer");
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let plain_grid = plain
            .read_with(cx, |viewer, _| viewer.grid().cloned())
            .expect("the grid");
        assert!(!plain_grid.read_with(cx, |grid, cx| grid.offers_focus_for_test(cx)));
    }
}
