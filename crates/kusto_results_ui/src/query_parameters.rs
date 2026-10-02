//! Finds and reads the query parameter profiles that apply to a query file.
//!
//! A query file has its own profiles in an adjacent `<name>.parameters.yaml` when that exists,
//! and otherwise shares the project's `.kusto/parameters.yaml`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use fs::Fs;
use gpui::{
    Action, App, AsyncWindowContext, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Task, WeakEntity, actions,
};
use gpui_util::ResultExt as _;
use kusto_client::{
    ParameterProfiles, WORKSPACE_PARAMETERS_PATH, sidecar_path, template, with_active_profile,
};
use picker::{Picker, PickerDelegate};
use project::Project;
use schemars::JsonSchema;
use serde::Deserialize;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, OpenOptions, OpenVisible, Workspace};

actions!(
    kusto,
    [
        /// Chooses which set of query parameter values queries run with.
        SelectParameterProfile,
        /// Opens the query parameter profiles that every query of the project shares.
        OpenParameters,
        /// Opens the query parameter profiles of this query file alone.
        OpenQueryParameters,
    ]
);

/// Makes a profile the active one in the profiles file that is open, from the lens above it.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = kusto)]
#[serde(deny_unknown_fields)]
pub struct MakeParameterProfileActive {
    pub name: String,
}

pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &SelectParameterProfile, window, cx| {
        select_parameter_profile(workspace, window, cx)
    });
    workspace.register_action(|workspace, _: &OpenParameters, window, cx| {
        open_parameters(workspace, ParameterFileKind::Project, window, cx)
    });
    workspace.register_action(|workspace, _: &OpenQueryParameters, window, cx| {
        open_parameters(workspace, ParameterFileKind::Query, window, cx)
    });
    workspace.register_action(|workspace, action: &MakeParameterProfileActive, _, cx| {
        make_profile_active(workspace, &action.name, cx)
    });
}

/// Whether a file is one of query parameter profiles: `parameters.yaml` or `<name>.parameters.yaml`.
fn is_profiles_file_name(name: &str) -> bool {
    name == "parameters.yaml" || name.ends_with(".parameters.yaml")
}

/// The smallest edit that turns `old` into `new`: the range of `old` to replace, and with what.
/// Changing only the lines that differ keeps the cursor, the undo history and the scroll of an
/// open file where they are.
fn minimal_edit<'a>(old: &str, new: &'a str) -> (std::ops::Range<usize>, &'a str) {
    let mut start = 0;
    for ((index, old_character), new_character) in old.char_indices().zip(new.chars()) {
        if old_character != new_character {
            break;
        }
        start = index + old_character.len_utf8();
    }
    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > start && new_end > start {
        let (Some(old_character), Some(new_character)) = (
            old[..old_end].chars().next_back(),
            new[..new_end].chars().next_back(),
        ) else {
            break;
        };
        if old_character != new_character {
            break;
        }
        old_end -= old_character.len_utf8();
        new_end -= new_character.len_utf8();
    }
    (start..old_end, &new[start..new_end])
}

/// Changes the `active:` line of the open profiles file, in its buffer, and saves it, so that
/// the lenses, which follow the buffer, change with it.
fn make_profile_active(workspace: &mut Workspace, name: &str, cx: &mut Context<Workspace>) {
    if let Err(error) = change_active_profile(workspace, name, cx) {
        workspace.show_error(error, cx);
    }
}

fn change_active_profile(
    workspace: &mut Workspace,
    name: &str,
    cx: &mut Context<Workspace>,
) -> Result<()> {
    let editor = workspace
        .active_item_as::<Editor>(cx)
        .context("Open a parameters file to make a profile active in it.")?;
    let buffer = editor
        .read(cx)
        .buffer()
        .read(cx)
        .as_singleton()
        .context("Open a parameters file to make a profile active in it.")?;
    let is_profiles_file = buffer
        .read(cx)
        .file()
        .is_some_and(|file| is_profiles_file_name(file.file_name(cx)));
    if !is_profiles_file {
        return Err(anyhow!("This is not a parameters file."));
    }

    let text = buffer.read(cx).text();
    let changed = with_active_profile(&text, Some(name))
        .ok_or_else(|| anyhow!("This file has no profile {name}, or cannot be read."))?;
    if changed != text {
        let (range, replacement) = minimal_edit(&text, &changed);
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(range, replacement)], None, cx);
        });
    }
    workspace
        .project()
        .update(cx, |project, cx| project.save_buffer(buffer, cx))
        .detach_and_log_err(cx);
    Ok(())
}

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

#[derive(Clone, Copy)]
enum ParameterFileKind {
    Project,
    Query,
}

fn files_of_active_editor(
    workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> Result<ParameterFiles> {
    let editor = workspace
        .active_item_as::<Editor>(cx)
        .context("Open a Kusto query to use its parameters.")?;
    let project = workspace.project().clone();
    Ok(editor.update(cx, |editor, cx| {
        ParameterFiles::of_editor(editor, project.read(cx), cx)
    }))
}

fn select_parameter_profile(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let files = match files_of_active_editor(workspace, cx) {
        Ok(files) => files,
        Err(error) => return workspace.show_error(error, cx),
    };
    let fs = workspace.app_state().fs.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let loaded = load(fs.as_ref(), &files).await;
        workspace
            .update_in(cx, |workspace, window, cx| match loaded {
                Ok(loaded) => {
                    let weak_workspace = cx.weak_entity();
                    workspace.toggle_modal(window, cx, move |window, cx| {
                        ParameterProfileSelector::new(loaded, files, fs, weak_workspace, window, cx)
                    });
                }
                Err(error) => workspace.show_error(error, cx),
            })
            .log_err();
    })
    .detach();
}

fn open_parameters(
    workspace: &mut Workspace,
    kind: ParameterFileKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let files = match files_of_active_editor(workspace, cx) {
        Ok(files) => files,
        Err(error) => return workspace.show_error(error, cx),
    };
    let fs = workspace.app_state().fs.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let opened = open_parameter_file(&workspace, fs, files, kind, cx).await;
        if let Err(error) = opened {
            workspace
                .update(cx, |workspace, cx| workspace.show_error(error, cx))
                .log_err();
        }
    })
    .detach();
}

/// Opens a profiles file, making it first from the profiles that apply when it does not exist.
async fn open_parameter_file(
    workspace: &WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    files: ParameterFiles,
    kind: ParameterFileKind,
    cx: &mut AsyncWindowContext,
) -> Result<()> {
    let path = match kind {
        ParameterFileKind::Project => files
            .workspace
            .clone()
            .context("Open a project folder to keep parameter profiles in it.")?,
        ParameterFileKind::Query => files
            .sidecar
            .clone()
            .context("Save the query as a .kql file to give it parameter profiles of its own.")?,
    };
    if !fs.is_file(&path).await {
        let starting_point = load(fs.as_ref(), &files).await?;
        if let Some(folder) = path.parent() {
            fs.create_dir(folder).await?;
        }
        fs.write(&path, template(&starting_point.profiles).as_bytes())
            .await
            .with_context(|| format!("Could not create {}", path.display()))?;
    }
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
        .await?;
    Ok(())
}

/// Writes which profile is active into the file the profiles came from.
async fn write_active_profile(fs: Arc<dyn Fs>, path: PathBuf, name: Option<String>) -> Result<()> {
    let text = fs
        .load(&path)
        .await
        .with_context(|| format!("Could not read {}", path.display()))?;
    let changed = with_active_profile(&text, name.as_deref()).ok_or_else(|| {
        anyhow!(
            "{} has no profile {}",
            path.display(),
            name.as_deref().unwrap_or("to change")
        )
    })?;
    fs.write(&path, changed.as_bytes())
        .await
        .with_context(|| format!("Could not write {}", path.display()))
}

struct ParameterProfileSelector {
    picker: Entity<Picker<ParameterProfileDelegate>>,
}

impl ParameterProfileSelector {
    fn new(
        loaded: LoadedProfiles,
        files: ParameterFiles,
        fs: Arc<dyn Fs>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = ParameterProfileDelegate::entries(&loaded);
        let delegate = ParameterProfileDelegate {
            selector: cx.entity().downgrade(),
            workspace,
            fs,
            files,
            loaded,
            matches: entries.clone(),
            entries,
            selected_index: 0,
        };
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for ParameterProfileSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

impl Focusable for ParameterProfileSelector {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for ParameterProfileSelector {}
impl ModalView for ParameterProfileSelector {}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Profile(String),
    NoProfile,
    EditProfiles,
}

impl Entry {
    fn label(&self) -> &str {
        match self {
            Entry::Profile(name) => name,
            Entry::NoProfile => "No active profile",
            Entry::EditProfiles => "Edit profiles…",
        }
    }
}

struct ParameterProfileDelegate {
    selector: WeakEntity<ParameterProfileSelector>,
    workspace: WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    files: ParameterFiles,
    loaded: LoadedProfiles,
    entries: Vec<Entry>,
    matches: Vec<Entry>,
    selected_index: usize,
}

impl ParameterProfileDelegate {
    fn entries(loaded: &LoadedProfiles) -> Vec<Entry> {
        loaded
            .profiles
            .profiles
            .iter()
            .map(|profile| Entry::Profile(profile.name.clone()))
            .chain(loaded.profiles.active.is_some().then_some(Entry::NoProfile))
            .chain([Entry::EditProfiles])
            .collect()
    }

    fn is_active(&self, entry: &Entry) -> bool {
        matches!(entry, Entry::Profile(name) if self.loaded.profiles.active.as_deref() == Some(name))
    }
}

impl PickerDelegate for ParameterProfileDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "query parameter profile selector"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select a query parameter profile…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, _: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.matches = self
            .entries
            .iter()
            .filter(|entry| entry.label().to_lowercase().contains(&query))
            .cloned()
            .collect();
        self.selected_index = 0;
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self.matches.get(self.selected_index).cloned() else {
            return;
        };
        let workspace = self.workspace.clone();
        let fs = self.fs.clone();
        match entry {
            Entry::EditProfiles => {
                let files = self.files.clone();
                let kind = if files.sidecar.is_some() && self.loaded.path == files.sidecar {
                    ParameterFileKind::Query
                } else {
                    ParameterFileKind::Project
                };
                cx.spawn_in(window, async move |_, cx| {
                    let opened = open_parameter_file(&workspace, fs, files, kind, cx).await;
                    report(&workspace, opened, cx);
                })
                .detach();
            }
            Entry::Profile(_) | Entry::NoProfile => {
                let Some(path) = self.loaded.path.clone() else {
                    return;
                };
                let name = match entry {
                    Entry::Profile(name) => Some(name),
                    _ => None,
                };
                cx.spawn_in(window, async move |_, cx| {
                    let written = write_active_profile(fs, path, name).await;
                    report(&workspace, written, cx);
                })
                .detach();
            }
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selector
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.matches.get(ix)?;
        let mut item = ListItem::new(ix)
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(Label::new(entry.label().to_string()));
        if self.is_active(entry) {
            item = item.end_slot(Icon::new(IconName::Check).color(Color::Muted));
        }
        Some(item)
    }
}

fn report(workspace: &WeakEntity<Workspace>, outcome: Result<()>, cx: &mut AsyncWindowContext) {
    if let Err(error) = outcome {
        workspace
            .update(cx, |workspace, cx| workspace.show_error(error, cx))
            .log_err();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use serde_json::json;

    use super::*;
    use crate::run_query::tests::setup_in;

    const SHARED: &str =
        "# who is on call\nactive: A\nprofiles:\n  A:\n    raid: from-a\n  B:\n    raid: from-b\n";
    const DECLARING: &str = "declare query_parameters(raid:string);\nprint raid\n";

    fn fs_of(workspace: &Entity<Workspace>, cx: &mut gpui::VisualTestContext) -> Arc<dyn Fs> {
        workspace.read_with(cx, |workspace, _| workspace.app_state().fs.clone())
    }

    fn selector(
        workspace: &Entity<Workspace>,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<ParameterProfileSelector> {
        workspace
            .read_with(cx, |workspace, cx| {
                workspace.active_modal::<ParameterProfileSelector>(cx)
            })
            .expect("the profile selector is open")
    }

    fn choices(
        selector: &Entity<ParameterProfileSelector>,
        cx: &mut gpui::VisualTestContext,
    ) -> Vec<String> {
        selector.read_with(cx, |selector, cx| {
            selector
                .picker
                .read(cx)
                .delegate
                .matches
                .iter()
                .map(|entry| entry.label().to_string())
                .collect()
        })
    }

    fn confirm(
        selector: &Entity<ParameterProfileSelector>,
        label: &str,
        cx: &mut gpui::VisualTestContext,
    ) {
        let picker = selector.read_with(cx, |selector, _| selector.picker.clone());
        picker.update_in(cx, |picker, window, cx| {
            let index = picker
                .delegate
                .matches
                .iter()
                .position(|entry| entry.label() == label)
                .expect("the choice is listed");
            picker.delegate.set_selected_index(index, window, cx);
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_selector_lists_the_profiles_and_makes_the_chosen_one_active(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({ ".kusto": { "parameters.yaml": SHARED } }),
        )
        .await;
        workspace.update_in(cx, |workspace, window, cx| {
            select_parameter_profile(workspace, window, cx)
        });
        cx.run_until_parked();

        let selector = selector(&workspace, cx);
        assert_eq!(
            choices(&selector, cx),
            ["A", "B", "No active profile", "Edit profiles…"]
        );
        confirm(&selector, "B", cx);

        let fs = fs_of(&workspace, cx);
        let text = fs
            .load(std::path::Path::new("/root/.kusto/parameters.yaml"))
            .await
            .expect("the file is still there");
        assert_eq!(
            text,
            SHARED.replace("active: A", "active: \"B\""),
            "only the active line changes"
        );
        assert!(
            workspace.read_with(cx, |workspace, cx| workspace
                .active_modal::<ParameterProfileSelector>(cx)
                .is_none()),
            "the selector closes"
        );
    }

    #[gpui::test]
    async fn the_selector_can_switch_the_profiles_off_and_filters_as_you_type(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({ ".kusto": { "parameters.yaml": SHARED } }),
        )
        .await;
        workspace.update_in(cx, |workspace, window, cx| {
            select_parameter_profile(workspace, window, cx)
        });
        cx.run_until_parked();
        let selector = selector(&workspace, cx);

        let picker = selector.read_with(cx, |selector, _| selector.picker.clone());
        picker.update_in(cx, |picker, window, cx| {
            picker.set_query("no act", window, cx)
        });
        cx.run_until_parked();
        assert_eq!(choices(&selector, cx), ["No active profile"]);
        confirm(&selector, "No active profile", cx);

        let text = fs_of(&workspace, cx)
            .load(std::path::Path::new("/root/.kusto/parameters.yaml"))
            .await
            .expect("the file is still there");
        assert_eq!(text, SHARED.replace("active: A", "active: null"));
    }

    #[gpui::test]
    async fn without_a_profiles_file_the_selector_offers_to_make_one(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(cx, 200, "", DECLARING, json!({})).await;
        workspace.update_in(cx, |workspace, window, cx| {
            select_parameter_profile(workspace, window, cx)
        });
        cx.run_until_parked();
        let selector = selector(&workspace, cx);
        assert_eq!(choices(&selector, cx), ["Edit profiles…"]);

        confirm(&selector, "Edit profiles…", cx);

        let fs = fs_of(&workspace, cx);
        let text = fs
            .load(std::path::Path::new("/root/.kusto/parameters.yaml"))
            .await
            .expect("the file was made");
        let made = ParameterProfiles::parse(&text).expect("it is a valid file");
        assert_eq!(made.profiles.len(), 1, "it starts with an example");
        let open_path = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .map(|path| path.path.as_unix_str().to_string())
        });
        assert_eq!(open_path.as_deref(), Some(".kusto/parameters.yaml"));
    }

    #[gpui::test]
    async fn a_query_files_own_profiles_start_from_the_projects(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({ ".kusto": { "parameters.yaml": SHARED } }),
        )
        .await;
        workspace.update_in(cx, |workspace, window, cx| {
            open_parameters(workspace, ParameterFileKind::Query, window, cx)
        });
        cx.run_until_parked();

        let text = fs_of(&workspace, cx)
            .load(std::path::Path::new("/root/queries.parameters.yaml"))
            .await
            .expect("the file was made");
        let made = ParameterProfiles::parse(&text).expect("it is a valid file");
        let shared = ParameterProfiles::parse(SHARED).expect("a valid file");
        assert_eq!(made, shared);
    }

    #[gpui::test]
    async fn the_selector_edits_the_file_the_profiles_came_from(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({
                "queries.parameters.yaml": "active: Mine\nprofiles:\n  Mine:\n    raid: x\n",
                ".kusto": { "parameters.yaml": SHARED },
            }),
        )
        .await;
        workspace.update_in(cx, |workspace, window, cx| {
            select_parameter_profile(workspace, window, cx)
        });
        cx.run_until_parked();
        let selector = selector(&workspace, cx);
        assert_eq!(
            choices(&selector, cx),
            ["Mine", "No active profile", "Edit profiles…"]
        );

        confirm(&selector, "Edit profiles…", cx);
        let open_path = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx))
                .map(|path| path.path.as_unix_str().to_string())
        });
        assert_eq!(open_path.as_deref(), Some("queries.parameters.yaml"));
    }

    #[test]
    fn the_smallest_edit_changes_only_what_differs() {
        let old = "# note\nactive: A\nprofiles:\n  A: {}\n";
        let new = "# note\nactive: \"B\"\nprofiles:\n  A: {}\n";
        let (range, replacement) = minimal_edit(old, new);
        assert_eq!(&old[range.clone()], "A");
        assert_eq!(replacement, "\"B\"");
        let mut applied = old.to_string();
        applied.replace_range(range, replacement);
        assert_eq!(applied, new);

        assert_eq!(minimal_edit("same", "same"), (4..4, ""));
        assert_eq!(minimal_edit("", "new"), (0..0, "new"));
        assert_eq!(minimal_edit("gone", ""), (0..4, ""));
    }

    #[test]
    fn the_smallest_edit_never_cuts_a_character_in_two() {
        let (range, replacement) = minimal_edit("name: é\n", "name: ê\n");
        assert_eq!(&"name: é\n"[range], "é");
        assert_eq!(replacement, "ê");
        let (range, replacement) = minimal_edit("a·b", "a·c");
        assert_eq!(&"a·b"[range], "b");
        assert_eq!(replacement, "c");
    }

    #[test]
    fn only_profile_files_are_profile_file_names() {
        assert!(is_profiles_file_name("parameters.yaml"));
        assert!(is_profiles_file_name("incident.parameters.yaml"));
        assert!(!is_profiles_file_name("config.yaml"));
        assert!(!is_profiles_file_name("parameters.yml"));
        assert!(!is_profiles_file_name("queries.kql"));
    }

    async fn open_in_editor(
        workspace: &Entity<Workspace>,
        name: &'static str,
        cx: &mut gpui::VisualTestContext,
    ) -> Entity<Editor> {
        let worktree_id = workspace.read_with(cx, |workspace, cx| {
            workspace
                .project()
                .read(cx)
                .worktrees(cx)
                .next()
                .map(|worktree| worktree.read(cx).id())
                .expect("the test project has a worktree")
        });
        let path = project::ProjectPath {
            worktree_id,
            path: util::rel_path::RelPath::new(
                std::path::Path::new(name),
                util::paths::PathStyle::Unix,
            )
            .expect("a relative path")
            .into_arc(),
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path(path, None, true, window, cx)
            })
            .await
            .expect("the file opens")
            .downcast::<Editor>()
            .expect("an editor")
    }

    #[gpui::test]
    async fn making_a_profile_active_edits_the_open_buffer_and_saves_the_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({ ".kusto": { "parameters.yaml": SHARED } }),
        )
        .await;
        let editor = open_in_editor(&workspace, ".kusto/parameters.yaml", cx).await;
        // An edit of the person's own, not saved yet, which must stay.
        editor.update_in(cx, |editor, window, cx| {
            editor.set_text(format!("{SHARED}# extra\n"), window, cx)
        });

        workspace.update(cx, |workspace, cx| make_profile_active(workspace, "B", cx));
        cx.run_until_parked();

        let expected = format!("{}# extra\n", SHARED.replace("active: A", "active: \"B\""));
        let in_buffer = editor.update(cx, |editor, cx| editor.text(cx));
        assert_eq!(in_buffer, expected);
        let on_disk = fs_of(&workspace, cx)
            .load(std::path::Path::new("/root/.kusto/parameters.yaml"))
            .await
            .expect("the file is there");
        assert_eq!(
            on_disk, expected,
            "the buffer was saved, with its other edit"
        );
    }

    #[gpui::test]
    async fn a_profile_the_file_does_not_have_changes_nothing(cx: &mut TestAppContext) {
        let (workspace, _editor, _sent, cx) = setup_in(
            cx,
            200,
            "",
            DECLARING,
            json!({ ".kusto": { "parameters.yaml": SHARED } }),
        )
        .await;
        let editor = open_in_editor(&workspace, ".kusto/parameters.yaml", cx).await;
        workspace.update(cx, |workspace, cx| {
            make_profile_active(workspace, "Nope", cx)
        });
        cx.run_until_parked();

        assert_eq!(editor.update(cx, |editor, cx| editor.text(cx)), SHARED);
        let on_disk = fs_of(&workspace, cx)
            .load(std::path::Path::new("/root/.kusto/parameters.yaml"))
            .await
            .expect("the file is there");
        assert_eq!(on_disk, SHARED);
    }

    #[gpui::test]
    async fn a_file_that_is_not_a_profiles_file_is_never_edited(cx: &mut TestAppContext) {
        let (workspace, editor, _sent, cx) = setup_in(cx, 200, "", SHARED, json!({})).await;
        // The query file holds YAML that would otherwise be a valid profiles file.
        workspace.update(cx, |workspace, cx| make_profile_active(workspace, "B", cx));
        cx.run_until_parked();
        assert_eq!(editor.update(cx, |editor, cx| editor.text(cx)), SHARED);
    }
}
