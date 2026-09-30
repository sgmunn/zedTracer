//! Opening a `.ktt` or `.kqr` file in a tab: the project item that loads and parses it, and the
//! tab that shows its first table in a results grid.

use std::ffi::OsStr;
use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Task, Window, div,
};
use kusto_results::ResultSet;
use project::{Project, ProjectEntryId, ProjectPath};
use ui::{Icon, IconName};
use util::rel_path::RelPath;
use workspace::Pane;
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ProjectItem as WorkspaceProjectItem};

use crate::grid::ResultGrid;
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
            Ok(cx.new(|_| Self {
                project_path,
                file_name,
                result: Arc::new(result),
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
        Self {
            focus_handle: cx.focus_handle(),
            results_file: item,
            grid,
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

    use fs::FakeFs;
    use gpui::TestAppContext;
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
}
