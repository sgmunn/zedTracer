//! Finds and reads the query parameter profiles that apply to a query file.
//!
//! A query file has its own profiles in an adjacent `<name>.parameters.yaml` when that exists,
//! and otherwise shares the project's `.kusto/parameters.yaml`.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use editor::Editor;
use fs::Fs;
use gpui::App;
use kusto_client::{ParameterProfiles, WORKSPACE_PARAMETERS_PATH, sidecar_path};
use project::Project;

/// Where the profiles for the query file of an editor would be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParameterFiles {
    pub sidecar: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

/// The profiles that apply, and the file they came from, which is `None` when there is no file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoadedProfiles {
    pub profiles: ParameterProfiles,
    pub path: Option<PathBuf>,
}

impl ParameterFiles {
    pub fn of_editor(editor: &Editor, project: &Project, cx: &App) -> Self {
        let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
            return Self::default();
        };
        let Some(file) = buffer.read(cx).file() else {
            return Self::default();
        };
        Self {
            sidecar: file
                .as_local()
                .and_then(|file| sidecar_path(&file.abs_path(cx))),
            workspace: project
                .worktree_for_id(file.worktree_id(cx), cx)
                .map(|worktree| worktree.read(cx).abs_path().join(WORKSPACE_PARAMETERS_PATH)),
        }
    }
}

/// Reads the profiles that apply. A file that exists but cannot be read is an error rather than
/// no profiles, so that a query is not run with a value missing because of a typo.
pub(crate) async fn load(fs: &dyn Fs, files: &ParameterFiles) -> Result<LoadedProfiles> {
    for path in [&files.sidecar, &files.workspace].into_iter().flatten() {
        if !fs.is_file(path).await {
            continue;
        }
        let text = fs
            .load(path)
            .await
            .with_context(|| format!("Could not read {}", path.display()))?;
        let profiles = ParameterProfiles::parse(&text)
            .with_context(|| format!("Could not read {}", path.display()))?;
        return Ok(LoadedProfiles {
            profiles,
            path: Some(path.clone()),
        });
    }
    Ok(LoadedProfiles::default())
}
