use gpui::{
    Action, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, Pixels, Render, SharedString, Task, WeakEntity,
    Window, actions, div, px,
};
use project::{Project, ProjectEntryId, ProjectPath};
use ui::{IconName, Label, prelude::*};
use util::rel_path::RelPath;
use workspace::{
    Item, Pane, Panel, Workspace,
    dock::{DockPosition, PanelEvent},
    item::{ItemBufferKind, ProjectItem as WorkspaceProjectItem},
};

actions!(investigation, [ToggleInvestigation]);

pub fn init(cx: &mut App) {
    workspace::register_project_item::<TraceViewer>(cx);
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleInvestigation, window, cx| {
            if !workspace.toggle_panel_focus::<InvestigationPanel>(window, cx) {
                workspace.close_panel::<InvestigationPanel>(window, cx);
            }
        });
    })
    .detach();
}

pub struct InvestigationPanel {
    focus_handle: FocusHandle,
    call_tree_expanded: bool,
}

impl InvestigationPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |_workspace, _window, cx| {
            cx.new(|cx| Self {
                focus_handle: cx.focus_handle(),
                call_tree_expanded: true,
            })
        })
    }
}

impl EventEmitter<PanelEvent> for InvestigationPanel {}

impl Focusable for InvestigationPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for InvestigationPanel {
    fn persistent_name() -> &'static str {
        "Investigation Panel"
    }

    fn panel_key() -> &'static str {
        "InvestigationPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Right
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(320.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ListTree)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Investigation")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleInvestigation)
    }

    fn starts_open(&self, _: &Window, _: &App) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        7
    }
}

impl Render for InvestigationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(Label::new("INVESTIGATION"))
            .child(Label::new("Workspace Provisioning Regression"))
            .child(Label::new("Findings"))
            .child(Label::new("  ⚠ SQL connection reset"))
            .child(Label::new("  ⚠ Retry consumed 8.1 s"))
            .child(Label::new("Evidence"))
            .child(Label::new("  provisioning.trace"))
            .child(
                div()
                    .id("investigation-call-tree")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.call_tree_expanded = !this.call_tree_expanded;
                        cx.notify();
                    }))
                    .child(Label::new(if self.call_tree_expanded {
                        "▾ Call tree"
                    } else {
                        "▸ Call tree"
                    })),
            )
            .when(self.call_tree_expanded, |this| {
                this.child(Label::new("  Request                 12.4 s"))
                    .child(Label::new("    Provision            11.9 s"))
                    .child(Label::new("      Execute SQL         8.1 s  ⚠"))
            })
    }
}

pub struct TraceFile {
    project_path: ProjectPath,
    file_name: String,
}

fn is_trace_path(path: &RelPath) -> bool {
    path.extension() == Some("trace")
}

impl project::ProjectItem for TraceFile {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<anyhow::Result<Entity<Self>>>> {
        let is_trace_file = is_trace_path(path.path.as_ref())
            || (path.path.as_ref() == RelPath::empty()
                && project
                    .read(cx)
                    .worktree_for_id(path.worktree_id, cx)
                    .is_some_and(|worktree| {
                        worktree.read(cx).abs_path().extension()
                            == Some(std::ffi::OsStr::new("trace"))
                    }));
        is_trace_file.then(|| {
            let project_path = path.clone();
            let file_name = project
                .read(cx)
                .absolute_path(path, cx)
                .and_then(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "Trace".to_string());
            cx.spawn(async move |cx| {
                Ok(cx.new(|_| Self {
                    project_path,
                    file_name,
                }))
            })
        })
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

pub struct TraceViewer {
    focus_handle: FocusHandle,
    trace_file: Entity<TraceFile>,
}

impl EventEmitter<()> for TraceViewer {}

impl Focusable for TraceViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for TraceViewer {
    type Event = ();

    fn tab_content_text(&self, _: usize, cx: &App) -> SharedString {
        self.trace_file.read(cx).file_name.clone().into()
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        callback: &mut dyn FnMut(EntityId, &dyn project::ProjectItem),
    ) {
        callback(self.trace_file.entity_id(), self.trace_file.read(cx));
    }

    fn buffer_kind(&self, _: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }
}

impl WorkspaceProjectItem for TraceViewer {
    type Item = TraceFile;

    fn for_project_item(
        _: Entity<Project>,
        _: Option<&Pane>,
        item: Entity<Self::Item>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            trace_file: item,
        }
    }
}

impl Render for TraceViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .child(Label::new(self.tab_content_text(0, cx)))
            .child(Label::new("Overview     Timeline     Call Tree     Events"))
            .child(Label::new("Request                      12.4 s"))
            .child(Label::new("  Provision                  11.9 s"))
            .child(Label::new("    Execute SQL               8.1 s  ⚠"))
            .child(Label::new("SqlException 10054: Connection reset by peer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use util::paths::PathStyle;

    #[test]
    fn routes_only_trace_files_to_trace_viewer() -> anyhow::Result<()> {
        for (name, expected) in [
            ("sample.trace", true),
            ("sample.trace.json", false),
            ("sample.txt", false),
            ("sample.traceback", false),
        ] {
            let path = RelPath::new(Path::new(name), PathStyle::Unix)?;
            assert_eq!(is_trace_path(&path), expected, "{name}");
        }
        Ok(())
    }
}
