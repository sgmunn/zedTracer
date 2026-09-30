//! Opening a `.ktt` or `.kqr` file in a tab: the project item that loads and parses it, and the
//! tab that shows its first table in a results grid.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use encoding_rs::Encoding;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, WeakEntity, Window, div,
};
use gpui_util::ResultExt as _;
use kusto_results::{ResultSet, TableView};
use project::{Project, ProjectEntryId, ProjectPath};
use rope::Rope;
use text::LineEnding;
use ui::{Icon, IconName};
use util::rel_path::RelPath;
use workspace::Pane;
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ProjectItem as WorkspaceProjectItem};
use worktree::Worktree;

use crate::grid::{ResultGrid, ResultGridEvent};
use crate::row_details_panel::{RowDetailsPanel, ToggleRowDetails};

pub fn init(cx: &mut App) {
    workspace::register_project_item::<ResultsViewer>(cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
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
    /// How the file was stored, so that writing it back keeps its line endings and encoding.
    storage: Option<FileStorage>,
    save_delay: Option<Task<()>>,
    write_in_flight: Option<Task<()>>,
    changed_during_write: bool,
}

struct FileStorage {
    worktree: WeakEntity<Worktree>,
    line_ending: LineEnding,
    encoding: &'static Encoding,
    has_bom: bool,
}

/// Layout changes are written after the user has stopped for this long.
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
            this.update(cx, |this, cx| this.write(cx)).log_err();
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
            let loaded = load.await.with_context(|| format!("reading {file_name}"))?;
            let text = loaded.text.to_string();
            let result = cx
                .background_spawn(async move { ResultSet::from_json(&text) })
                .await
                .with_context(|| format!("parsing {file_name}"))?;
            let storage = loaded.is_writable.then(|| FileStorage {
                worktree: worktree.downgrade(),
                line_ending: loaded.line_ending,
                encoding: loaded.encoding,
                has_bom: loaded.has_bom,
            });
            Ok(cx.new(|_| Self {
                project_path,
                file_name,
                result: Arc::new(result),
                storage,
                save_delay: None,
                write_in_flight: None,
                changed_during_write: false,
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

pub struct ResultsViewer {
    focus_handle: FocusHandle,
    results_file: Entity<ResultsFile>,
    grid: Option<Entity<ResultGrid>>,
    _grid_subscription: Option<Subscription>,
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
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let result = item.read(cx).result.clone();
        let grid = (!result.tables.is_empty()).then(|| cx.new(|cx| ResultGrid::new(result, 0, cx)));
        let grid_subscription = grid.as_ref().map(|grid| {
            cx.subscribe(grid, |this, _, event: &ResultGridEvent, cx| {
                if let ResultGridEvent::LayoutChanged(layout) = event {
                    this.results_file
                        .update(cx, |file, cx| file.save_layout(layout.clone(), cx));
                }
            })
        });
        Self {
            focus_handle: cx.focus_handle(),
            results_file: item,
            grid,
            _grid_subscription: grid_subscription,
        }
    }
}

impl Render for ResultsViewer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.grid {
            Some(grid) => div().size_full().child(grid.clone()),
            None => div()
                .p_4()
                .child(ui::Label::new("This file contains no tables.")),
        };
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(body)
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

        let broken = ProjectPath {
            worktree_id,
            path: RelPath::new(Path::new("broken.ktt"), util::paths::PathStyle::Unix)
                .expect("relative path")
                .into_arc(),
        };
        let opened = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(broken, None, true, window, cx)
            })
            .await;
        assert!(opened.is_err(), "a file that does not parse is not opened");
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
        cx.executor().advance_clock(SAVE_DELAY * 2);
        cx.run_until_parked();

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
}
