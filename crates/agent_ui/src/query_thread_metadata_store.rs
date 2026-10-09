use std::fmt;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use collections::{HashMap, HashSet};
use db::{
    sqlez::{
        bindable::Column, domain::Domain, statement::Statement,
        thread_safe_connection::ThreadSafeConnection,
    },
    sqlez_macros::sql,
};
use futures::{FutureExt, future::Shared};
use gpui::{AppContext as _, Entity, Global, Task};
use ui::{App, Context, SharedString};
use util::ResultExt as _;
use workspace::PathList;

use crate::thread_metadata_store::WorktreePaths;

pub fn init(cx: &mut App) {
    QueryThreadMetadataStore::init_global(cx);
}

/// What a new query thread is called until the user renames it.
pub const DEFAULT_QUERY_THREAD_TITLE: &str = "Kusto query";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct QueryThreadId(uuid::Uuid);

impl QueryThreadId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn to_key_string(self) -> String {
        self.0.hyphenated().to_string()
    }

    pub fn from_key_string(key: &str) -> anyhow::Result<Self> {
        Ok(Self(uuid::Uuid::parse_str(key)?))
    }

    /// The query file of the thread, which is named by its id.
    pub fn query_file(self) -> std::path::PathBuf {
        kusto_results_ui::threads_folder().join(format!("{}.kql", self.to_key_string()))
    }
}

impl fmt::Display for QueryThreadId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

struct GlobalQueryThreadMetadataStore(Entity<QueryThreadMetadataStore>);
impl Global for GlobalQueryThreadMetadataStore {}

#[cfg(any(test, feature = "test-support"))]
pub struct TestQueryThreadMetadataDbName(pub String);
#[cfg(any(test, feature = "test-support"))]
impl Global for TestQueryThreadMetadataDbName {}

#[cfg(any(test, feature = "test-support"))]
impl TestQueryThreadMetadataDbName {
    pub fn global(cx: &App) -> String {
        cx.try_global::<Self>()
            .map(|global| global.0.clone())
            .unwrap_or_else(|| {
                let thread = std::thread::current();
                let test_name = thread.name().unwrap_or("unknown_test");
                format!("QUERY_THREAD_METADATA_DB_{}", test_name)
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryThreadMetadata {
    pub id: QueryThreadId,
    pub title: SharedString,
    pub created_at: DateTime<Utc>,
    pub worktree_paths: WorktreePaths,
}

impl QueryThreadMetadata {
    pub fn folder_paths(&self) -> &PathList {
        self.worktree_paths.folder_path_list()
    }

    pub fn main_worktree_paths(&self) -> &PathList {
        self.worktree_paths.main_worktree_path_list()
    }
}

pub struct QueryThreadMetadataStore {
    db: QueryThreadMetadataDb,
    query_threads: HashMap<QueryThreadId, QueryThreadMetadata>,
    query_threads_by_paths: HashMap<PathList, HashSet<QueryThreadId>>,
    query_threads_by_main_paths: HashMap<PathList, HashSet<QueryThreadId>>,
    reload_task: Option<Shared<Task<()>>>,
    pending_operations_tx: async_channel::Sender<DbOperation>,
    _db_operations_task: Task<()>,
}

#[derive(Debug, PartialEq)]
enum DbOperation {
    Upsert(QueryThreadMetadata),
    Delete(QueryThreadId),
}

impl DbOperation {
    fn id(&self) -> QueryThreadId {
        match self {
            DbOperation::Upsert(metadata) => metadata.id,
            DbOperation::Delete(id) => *id,
        }
    }
}

impl QueryThreadMetadataStore {
    #[cfg(not(any(test, feature = "test-support")))]
    pub fn init_global(cx: &mut App) {
        if cx.has_global::<GlobalQueryThreadMetadataStore>() {
            return;
        }

        let db = QueryThreadMetadataDb::global(cx);
        let store = cx.new(|cx| Self::new(db, cx));
        cx.set_global(GlobalQueryThreadMetadataStore(store));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn init_global(cx: &mut App) {
        let db_name = TestQueryThreadMetadataDbName::global(cx);
        let db = gpui::block_on(db::open_test_db::<QueryThreadMetadataDb>(&db_name));
        let store = cx.new(|cx| Self::new(QueryThreadMetadataDb(db), cx));
        cx.set_global(GlobalQueryThreadMetadataStore(store));
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalQueryThreadMetadataStore>()
            .map(|store| store.0.clone())
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalQueryThreadMetadataStore>().0.clone()
    }

    pub fn entry(&self, id: QueryThreadId) -> Option<&QueryThreadMetadata> {
        self.query_threads.get(&id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &QueryThreadMetadata> + '_ {
        self.query_threads.values()
    }

    pub fn reload_task(&self) -> Shared<Task<()>> {
        self.reload_task
            .clone()
            .unwrap_or_else(|| Task::ready(()).shared())
    }

    pub fn entries_for_path<'a>(
        &'a self,
        path_list: &PathList,
    ) -> impl Iterator<Item = &'a QueryThreadMetadata> + 'a {
        self.query_threads_by_paths
            .get(path_list)
            .into_iter()
            .flatten()
            .filter_map(|id| self.query_threads.get(id))
    }

    pub fn entries_for_main_worktree_path<'a>(
        &'a self,
        path_list: &PathList,
    ) -> impl Iterator<Item = &'a QueryThreadMetadata> + 'a {
        self.query_threads_by_main_paths
            .get(path_list)
            .into_iter()
            .flatten()
            .filter_map(|id| self.query_threads.get(id))
    }

    pub fn save(&mut self, metadata: QueryThreadMetadata, cx: &mut Context<Self>) {
        self.save_internal(metadata);
        cx.notify();
    }

    pub fn rename(&mut self, id: QueryThreadId, title: SharedString, cx: &mut Context<Self>) {
        let title = SharedString::from(title.trim().to_string());
        let Some(mut metadata) = self.entry(id).cloned() else {
            return;
        };
        if title.is_empty() || metadata.title == title {
            return;
        }
        metadata.title = title;
        self.save_internal(metadata);
        cx.notify();
    }

    fn save_internal(&mut self, metadata: QueryThreadMetadata) {
        if let Some(existing) = self.query_threads.get(&metadata.id) {
            if existing.folder_paths() != metadata.folder_paths()
                && let Some(ids) = self.query_threads_by_paths.get_mut(existing.folder_paths())
            {
                ids.remove(&metadata.id);
            }

            if existing.main_worktree_paths() != metadata.main_worktree_paths()
                && let Some(ids) = self
                    .query_threads_by_main_paths
                    .get_mut(existing.main_worktree_paths())
            {
                ids.remove(&metadata.id);
            }
        }

        self.cache_metadata(metadata.clone());
        self.pending_operations_tx
            .try_send(DbOperation::Upsert(metadata))
            .log_err();
    }

    fn cache_metadata(&mut self, metadata: QueryThreadMetadata) {
        self.query_threads.insert(metadata.id, metadata.clone());

        self.query_threads_by_paths
            .entry(metadata.folder_paths().clone())
            .or_default()
            .insert(metadata.id);

        if !metadata.main_worktree_paths().is_empty() {
            self.query_threads_by_main_paths
                .entry(metadata.main_worktree_paths().clone())
                .or_default()
                .insert(metadata.id);
        }
    }

    pub fn delete(&mut self, id: QueryThreadId, cx: &mut Context<Self>) {
        if let Some(removed) = self.query_threads.remove(&id) {
            if let Some(ids) = self.query_threads_by_paths.get_mut(removed.folder_paths()) {
                ids.remove(&id);
            }
            if !removed.main_worktree_paths().is_empty()
                && let Some(ids) = self
                    .query_threads_by_main_paths
                    .get_mut(removed.main_worktree_paths())
            {
                ids.remove(&id);
            }
        }
        self.pending_operations_tx
            .try_send(DbOperation::Delete(id))
            .log_err();
        cx.notify();
    }

    fn new(db: QueryThreadMetadataDb, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let _db_operations_task = cx.background_spawn({
            let db = db.clone();
            async move {
                while let Ok(first_update) = rx.recv().await {
                    let mut updates = vec![first_update];
                    while let Ok(update) = rx.try_recv() {
                        updates.push(update);
                    }
                    for operation in Self::dedup_db_operations(updates) {
                        match operation {
                            DbOperation::Upsert(metadata) => {
                                db.save(metadata).await.log_err();
                            }
                            DbOperation::Delete(id) => {
                                db.delete(id).await.log_err();
                            }
                        }
                    }
                }
            }
        });

        let mut this = Self {
            db,
            query_threads: HashMap::default(),
            query_threads_by_paths: HashMap::default(),
            query_threads_by_main_paths: HashMap::default(),
            reload_task: None,
            pending_operations_tx: tx,
            _db_operations_task,
        };
        this.reload(cx);
        this
    }

    fn dedup_db_operations(operations: Vec<DbOperation>) -> Vec<DbOperation> {
        let mut ops = HashMap::default();
        for operation in operations.into_iter().rev() {
            if ops.contains_key(&operation.id()) {
                continue;
            }
            ops.insert(operation.id(), operation);
        }
        ops.into_values().collect()
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let db = self.db.clone();
        self.reload_task = Some(
            cx.spawn(async move |this, cx| {
                let rows = cx
                    .background_spawn(async move {
                        db.list()
                            .context("Failed to fetch query thread metadata")
                    })
                    .await
                    .log_err()
                    .unwrap_or_default();

                this.update(cx, |this, cx| {
                    this.query_threads.clear();
                    this.query_threads_by_paths.clear();
                    this.query_threads_by_main_paths.clear();

                    for row in rows {
                        this.cache_metadata(row);
                    }

                    cx.notify();
                })
                .ok();
            })
            .shared(),
        );
    }
}

struct QueryThreadMetadataDb(ThreadSafeConnection);

impl Domain for QueryThreadMetadataDb {
    const NAME: &str = stringify!(QueryThreadMetadataDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE IF NOT EXISTS sidebar_query_threads(
            query_thread_id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            created_at TEXT NOT NULL,
            folder_paths TEXT,
            folder_paths_order TEXT,
            main_worktree_paths TEXT,
            main_worktree_paths_order TEXT
        ) STRICT;
    )];
}

db::static_connection!(QueryThreadMetadataDb, []);

impl QueryThreadMetadataDb {
    pub fn list(&self) -> anyhow::Result<Vec<QueryThreadMetadata>> {
        self.select::<QueryThreadMetadata>(
            "SELECT query_thread_id, title, created_at, folder_paths, folder_paths_order, \
            main_worktree_paths, main_worktree_paths_order \
            FROM sidebar_query_threads \
            ORDER BY created_at DESC",
        )?()
    }

    pub async fn save(&self, row: QueryThreadMetadata) -> anyhow::Result<()> {
        let id = row.id.to_key_string();
        let title = row.title.to_string();
        let created_at = row.created_at.to_rfc3339();
        let serialized = row.folder_paths().serialize();
        let (folder_paths, folder_paths_order) = if row.folder_paths().is_empty() {
            (None, None)
        } else {
            (Some(serialized.paths), Some(serialized.order))
        };
        let main_serialized = row.main_worktree_paths().serialize();
        let (main_worktree_paths, main_worktree_paths_order) =
            if row.main_worktree_paths().is_empty() {
                (None, None)
            } else {
                (Some(main_serialized.paths), Some(main_serialized.order))
            };

        self.write(move |conn| {
            let sql = "INSERT INTO sidebar_query_threads(query_thread_id, title, created_at, folder_paths, folder_paths_order, main_worktree_paths, main_worktree_paths_order) \
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                       ON CONFLICT(query_thread_id) DO UPDATE SET \
                           title = excluded.title, \
                           created_at = excluded.created_at, \
                           folder_paths = excluded.folder_paths, \
                           folder_paths_order = excluded.folder_paths_order, \
                           main_worktree_paths = excluded.main_worktree_paths, \
                           main_worktree_paths_order = excluded.main_worktree_paths_order";
            let mut stmt = Statement::prepare(conn, sql)?;
            let mut i = stmt.bind(&id, 1)?;
            i = stmt.bind(&title, i)?;
            i = stmt.bind(&created_at, i)?;
            i = stmt.bind(&folder_paths, i)?;
            i = stmt.bind(&folder_paths_order, i)?;
            i = stmt.bind(&main_worktree_paths, i)?;
            stmt.bind(&main_worktree_paths_order, i)?;
            stmt.exec()
        })
        .await
    }

    pub async fn delete(&self, id: QueryThreadId) -> anyhow::Result<()> {
        let id = id.to_key_string();
        self.write(move |conn| {
            let mut stmt = Statement::prepare(
                conn,
                "DELETE FROM sidebar_query_threads WHERE query_thread_id = ?",
            )?;
            stmt.bind(&id, 1)?;
            stmt.exec()
        })
        .await
    }
}

impl Column for QueryThreadMetadata {
    fn column(statement: &mut Statement, start_index: i32) -> anyhow::Result<(Self, i32)> {
        let (id, next): (String, i32) = Column::column(statement, start_index)?;
        let (title, next): (String, i32) = Column::column(statement, next)?;
        let (created_at, next): (String, i32) = Column::column(statement, next)?;
        let (folder_paths_str, next): (Option<String>, i32) = Column::column(statement, next)?;
        let (folder_paths_order_str, next): (Option<String>, i32) =
            Column::column(statement, next)?;
        let (main_worktree_paths_str, next): (Option<String>, i32) =
            Column::column(statement, next)?;
        let (main_worktree_paths_order_str, next): (Option<String>, i32) =
            Column::column(statement, next)?;

        let folder_paths = folder_paths_str
            .map(|paths| {
                PathList::deserialize(&util::path_list::SerializedPathList {
                    paths,
                    order: folder_paths_order_str.unwrap_or_default(),
                })
            })
            .unwrap_or_default();

        let main_worktree_paths = main_worktree_paths_str
            .map(|paths| {
                PathList::deserialize(&util::path_list::SerializedPathList {
                    paths,
                    order: main_worktree_paths_order_str.unwrap_or_default(),
                })
            })
            .unwrap_or_default();

        let worktree_paths = WorktreePaths::from_path_lists(main_worktree_paths, folder_paths)
            .unwrap_or_else(|_| WorktreePaths::default());

        Ok((
            QueryThreadMetadata {
                id: QueryThreadId::from_key_string(&id)?,
                title: SharedString::from(title),
                created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc),
                worktree_paths,
            },
            next,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use gpui::TestAppContext;

    use super::*;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            QueryThreadMetadataStore::init_global(cx);
        });
        cx.run_until_parked();
    }

    fn metadata(title: &str, worktree_paths: WorktreePaths) -> QueryThreadMetadata {
        QueryThreadMetadata {
            id: QueryThreadId::new(),
            title: SharedString::from(title.to_string()),
            created_at: Utc::now(),
            worktree_paths,
        }
    }

    #[test]
    fn a_threads_file_is_named_by_its_id() {
        let id = QueryThreadId::new();
        let file = id.query_file();
        assert_eq!(file.parent(), Some(kusto_results_ui::threads_folder().as_path()));
        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some(format!("{}.kql", id.to_key_string()).as_str())
        );
        assert_eq!(
            QueryThreadId::from_key_string(&id.to_key_string()).expect("the key parses"),
            id
        );
    }

    #[gpui::test]
    async fn a_saved_thread_is_found_by_its_folders_and_comes_back_from_the_database(
        cx: &mut TestAppContext,
    ) {
        init_test(cx);
        let folder_paths = PathList::new(&[Path::new("/repo")]);
        let saved = metadata(
            "Slow requests",
            WorktreePaths::from_folder_paths(&folder_paths),
        );

        cx.update(|cx| {
            QueryThreadMetadataStore::global(cx).update(cx, |store, cx| {
                store.save(saved.clone(), cx);
            });
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let store = QueryThreadMetadataStore::global(cx);
            let found: Vec<_> = store
                .read(cx)
                .entries_for_path(&folder_paths)
                .cloned()
                .collect();
            assert_eq!(found, vec![saved.clone()]);
            assert_eq!(
                store
                    .read(cx)
                    .entries_for_path(&PathList::new(&[Path::new("/other")]))
                    .count(),
                0
            );
        });

        let listed = cx
            .update(|cx| QueryThreadMetadataStore::global(cx).read(cx).db.clone())
            .list()
            .expect("the rows are read");
        assert_eq!(
            listed.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![saved.id]
        );
        assert_eq!(listed[0].title, saved.title);
        assert_eq!(listed[0].folder_paths(), &folder_paths);
    }

    #[gpui::test]
    async fn renaming_keeps_the_title_and_ignores_an_empty_one(cx: &mut TestAppContext) {
        init_test(cx);
        let saved = metadata(
            DEFAULT_QUERY_THREAD_TITLE,
            WorktreePaths::from_folder_paths(&PathList::default()),
        );
        let id = saved.id;
        let title_of = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                QueryThreadMetadataStore::global(cx)
                    .read(cx)
                    .entry(id)
                    .map(|entry| entry.title.to_string())
            })
        };

        cx.update(|cx| {
            QueryThreadMetadataStore::global(cx).update(cx, |store, cx| {
                store.save(saved, cx);
                store.rename(id, "  Slow requests  ".into(), cx);
            });
        });
        assert_eq!(title_of(cx).as_deref(), Some("Slow requests"));

        cx.update(|cx| {
            QueryThreadMetadataStore::global(cx).update(cx, |store, cx| {
                store.rename(id, "   ".into(), cx);
            });
        });
        assert_eq!(title_of(cx).as_deref(), Some("Slow requests"));
    }

    #[gpui::test]
    async fn a_deleted_thread_is_gone_from_the_store_and_the_database(cx: &mut TestAppContext) {
        init_test(cx);
        let folder_paths = PathList::new(&[Path::new("/repo")]);
        let saved = metadata("Gone", WorktreePaths::from_folder_paths(&folder_paths));
        let id = saved.id;

        cx.update(|cx| {
            QueryThreadMetadataStore::global(cx).update(cx, |store, cx| store.save(saved, cx));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            QueryThreadMetadataStore::global(cx).update(cx, |store, cx| store.delete(id, cx));
        });
        cx.run_until_parked();

        cx.update(|cx| {
            let store = QueryThreadMetadataStore::global(cx);
            assert!(store.read(cx).entry(id).is_none());
            assert_eq!(store.read(cx).entries_for_path(&folder_paths).count(), 0);
        });
        let listed = cx
            .update(|cx| QueryThreadMetadataStore::global(cx).read(cx).db.clone())
            .list()
            .expect("the rows are read");
        assert!(listed.is_empty());
    }
}
