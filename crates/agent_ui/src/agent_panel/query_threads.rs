//! Query threads in the agent panel: a Kusto query editor as the panel's surface.
//!
//! The thread's queries are a file named by its id, so closing a thread forgets the thread and
//! keeps the file.

use chrono::{DateTime, Utc};
use gpui::{App, Context, Entity, Window};
use kusto_results_ui::QueryThread;
use ui::SharedString;
use util::ResultExt as _;

use super::{AgentPanel, AgentPanelEvent, BaseView};
use crate::AgentThreadSource;
use crate::query_thread_metadata_store::{
    DEFAULT_QUERY_THREAD_TITLE, QueryThreadId, QueryThreadMetadata, QueryThreadMetadataStore,
};

pub(super) struct AgentQueryThread {
    pub(super) thread: Entity<QueryThread>,
    created_at: DateTime<Utc>,
}

impl AgentPanel {
    /// Query threads edit a file on this machine, so they need a project that is on it.
    pub fn supports_query_threads(&self, cx: &App) -> bool {
        self.has_open_project(cx) && self.project.read(cx).is_local()
    }

    pub fn active_query_thread_id(&self) -> Option<QueryThreadId> {
        match &self.base_view {
            BaseView::QueryThread { query_thread_id } => Some(*query_thread_id),
            _ => None,
        }
    }

    pub fn new_query_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.supports_query_threads(cx) {
            return;
        }
        self.open_query_thread(QueryThreadId::new(), None, true, window, cx);
    }

    /// Shows the thread a row of the sidebar stands for, opening its file if it is not open.
    pub fn restore_query_thread(
        &mut self,
        metadata: QueryThreadMetadata,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query_threads.contains_key(&metadata.id) {
            self.activate_query_thread(metadata.id, focus, window, cx);
            return;
        }
        if !self.supports_query_threads(cx) {
            return;
        }
        self.open_query_thread(metadata.id, Some(metadata.created_at), focus, window, cx);
    }

    pub fn activate_query_thread(
        &mut self,
        id: QueryThreadId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query_threads.contains_key(&id) {
            self.set_base_view(
                BaseView::QueryThread {
                    query_thread_id: id,
                },
                focus,
                window,
                cx,
            );
        }
    }

    /// Forgets the thread. Its file stays where it is.
    pub fn close_query_thread(
        &mut self,
        id: QueryThreadId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_active = self.active_query_thread_id() == Some(id);
        if self.query_threads.remove(&id).is_none() {
            return;
        }
        if let Some(store) = QueryThreadMetadataStore::try_global(cx) {
            store.update(cx, |store, cx| store.delete(id, cx));
        }
        if was_active {
            self.base_view = BaseView::Uninitialized;
            self.refresh_base_view_subscriptions(window, cx);
            self.activate_draft(false, AgentThreadSource::AgentPanel, window, cx);
        }
        cx.emit(AgentPanelEvent::EntryChanged);
        cx.notify();
    }

    /// Renames the thread if this panel has it, and says whether it does.
    pub fn rename_query_thread(
        &mut self,
        id: QueryThreadId,
        title: SharedString,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.query_threads.contains_key(&id) {
            return false;
        }
        if let Some(store) = QueryThreadMetadataStore::try_global(cx) {
            store.update(cx, |store, cx| store.rename(id, title, cx));
        }
        cx.emit(AgentPanelEvent::EntryChanged);
        cx.notify();
        true
    }

    fn open_query_thread(
        &mut self,
        id: QueryThreadId,
        created_at: Option<DateTime<Utc>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        let opening = QueryThread::open(
            workspace.clone(),
            self.project.clone(),
            self.fs.clone(),
            id.query_file(),
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match opening.await {
            Ok(thread) => {
                this.update_in(cx, |this, window, cx| {
                    this.insert_query_thread(id, thread, created_at, focus, window, cx)
                })
                .log_err();
            }
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| workspace.show_error(error, cx))
                    .log_err();
            }
        })
        .detach();
    }

    fn insert_query_thread(
        &mut self,
        id: QueryThreadId,
        thread: Entity<QueryThread>,
        created_at: Option<DateTime<Utc>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Two clicks on one row open the file twice; the second thread is dropped.
        if self.query_threads.contains_key(&id) {
            self.activate_query_thread(id, focus, window, cx);
            return;
        }
        self.query_threads.insert(
            id,
            AgentQueryThread {
                thread,
                created_at: created_at.unwrap_or_else(Utc::now),
            },
        );
        self.persist_query_thread_metadata(id, cx);
        self.activate_query_thread(id, focus, window, cx);
        cx.emit(AgentPanelEvent::EntryChanged);
        cx.notify();
    }

    fn persist_query_thread_metadata(&self, id: QueryThreadId, cx: &mut Context<Self>) {
        let Some(store) = QueryThreadMetadataStore::try_global(cx) else {
            return;
        };
        let Some(metadata) = self.query_thread_metadata(id, cx) else {
            return;
        };
        store.update(cx, |store, cx| store.save(metadata, cx));
    }

    fn query_thread_metadata(&self, id: QueryThreadId, cx: &App) -> Option<QueryThreadMetadata> {
        let query_thread = self.query_threads.get(&id)?;
        Some(QueryThreadMetadata {
            id,
            title: self.query_thread_title(id, cx),
            created_at: query_thread.created_at,
            worktree_paths: self.project.read(cx).worktree_paths(cx),
        })
    }

    pub(super) fn query_thread_title(&self, id: QueryThreadId, cx: &App) -> SharedString {
        QueryThreadMetadataStore::try_global(cx)
            .and_then(|store| store.read(cx).entry(id).map(|entry| entry.title.clone()))
            .unwrap_or_else(|| SharedString::new_static(DEFAULT_QUERY_THREAD_TITLE))
    }
}
