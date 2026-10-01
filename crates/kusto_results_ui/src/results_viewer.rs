//! Opening a `.ktt` or `.kqr` file in a tab: the project item that loads and parses it, and the
//! tab that shows its first table in a results grid.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use encoding_rs::Encoding;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, WeakEntity, Window, div,
};
use gpui_util::ResultExt as _;
use kusto_results::activity::{build_projection, has_activity_columns};
use kusto_results::{NoResultData, ResultSet, TableView};
use project::{Project, ProjectEntryId, ProjectPath};
use rope::Rope;
use text::LineEnding;
use ui::{Button, Icon, IconName, prelude::*};
use util::rel_path::RelPath;
use workspace::Pane;
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ProjectItem as WorkspaceProjectItem};
use worktree::Worktree;

use crate::grid::{ResultGrid, ResultGridEvent};
use crate::row_details_panel::{RowDetailsPanel, ToggleRowDetails};
use crate::structured_view::StructuredView;

pub fn init(cx: &mut App) {
    workspace::register_project_item::<ResultsViewer>(cx);
    cx.observe_new(|workspace: &mut Workspace, _, cx| {
        crate::run_query::register(workspace, cx);
        crate::results_panel::register(workspace);
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
}

/// The structured view is built when it is first asked for: the projection of a large trace
/// takes a moment, and most tabs never need it.
enum Structured {
    Building { _task: Task<()> },
    Ready(Entity<StructuredView>),
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
    _grid_subscription: Option<Subscription>,
    _structured_subscription: Option<Subscription>,
    _reload_subscription: Subscription,
}

impl EventEmitter<()> for ResultsViewer {}

impl Focusable for ResultsViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
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
            _grid_subscription: None,
            _structured_subscription: None,
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
        self.problem = item.read(cx).problem;
        self.grid = (self.problem.is_none() && !result.tables.is_empty())
            .then(|| cx.new(|cx| ResultGrid::new(result.clone(), 0, window, cx)));
        self._grid_subscription = self
            .grid
            .as_ref()
            .map(|grid| Self::save_layouts_of(grid, cx));
        self.can_show_structured =
            self.grid.is_some() && result.tables.first().is_some_and(has_activity_columns);
        // What the structured view was built from has changed.
        self.structured = None;
        self._structured_subscription = None;
        if !self.can_show_structured {
            self.mode = ViewMode::Data;
        } else if self.mode == ViewMode::Structured {
            self.build_structured(window, cx);
        }
    }

    fn save_layouts_of(grid: &Entity<ResultGrid>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(grid, |this, _, event: &ResultGridEvent, cx| {
            if let ResultGridEvent::LayoutChanged(layout) = event {
                this.results_file
                    .update(cx, |file, cx| file.save_layout(layout.clone(), cx));
            }
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

    fn set_mode(&mut self, mode: ViewMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == ViewMode::Structured && !self.can_show_structured {
            return;
        }
        self.mode = mode;
        if mode == ViewMode::Structured && self.structured.is_none() {
            self.build_structured(window, cx);
        }
        cx.notify();
    }

    /// Builds the activity projection away from the window, then the view.
    fn build_structured(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.results_file.read(cx).result.clone();
        self.structured = Some(Structured::Building {
            _task: cx.spawn_in(window, async move |this, cx| {
                let for_projection = result.clone();
                let projection = cx
                    .background_spawn(async move {
                        for_projection.tables.first().and_then(build_projection)
                    })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let Some(projection) = projection else {
                        this.structured = None;
                        this.can_show_structured = false;
                        this.mode = ViewMode::Data;
                        cx.notify();
                        return;
                    };
                    let view = cx
                        .new(|cx| StructuredView::new(result, 0, Arc::new(projection), window, cx));
                    let grid = view.read(cx).grid().clone();
                    this._structured_subscription = Some(Self::save_layouts_of(&grid, cx));
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
        let body = match (self.mode, &self.structured, &self.grid) {
            (ViewMode::Structured, Some(Structured::Ready(view)), _) => {
                div().size_full().child(view.clone())
            }
            (ViewMode::Structured, _, _) => div()
                .p_4()
                .child(ui::Label::new("Building the activity tree…")),
            (ViewMode::Data, _, Some(grid)) => div().size_full().child(grid.clone()),
            (ViewMode::Data, _, None) => div()
                .p_4()
                .child(ui::Label::new(self.problem.unwrap_or(NO_RESULT_DATA))),
        };
        let mode = self.mode;
        v_flex()
            .size_full()
            .track_focus(&self.focus_handle)
            .when(self.can_show_structured, |viewer| {
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
                        .child(
                            div().debug_selector(|| "structured-tab".to_string()).child(
                                Button::new("results-structured-tab", "Structured")
                                    .toggle_state(mode == ViewMode::Structured)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_mode(ViewMode::Structured, window, cx)
                                    })),
                            ),
                        ),
                )
            })
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
}
