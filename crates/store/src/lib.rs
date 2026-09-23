use codexflow_domain::{
    AppError, AttributedThread, IndexRun, IndexRunState, ListScopeStatus, LocalProject,
    ObservedRelation, Preferences, ProjectCatalog, ProjectSessions, SessionList, ThreadAttribution,
    ThreadMetadata,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct PreferenceStore {
    path: PathBuf,
}

pub struct SessionStore {
    path: PathBuf,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn merge_thread_metadata(previous: ThreadMetadata, incoming: ThreadMetadata) -> ThreadMetadata {
    // The two list scopes can return the same ID at different source versions.
    if previous.updated_at > incoming.updated_at {
        let mut latest = previous;
        latest.observed_at_unix_ms = latest.observed_at_unix_ms.max(incoming.observed_at_unix_ms);
        latest.missing_from_source = false;
        return latest;
    }
    if previous.updated_at < incoming.updated_at {
        return incoming;
    }

    // Within one refresh, the archived scope wins an equal-version duplicate.
    let same_refresh = previous.observed_at_unix_ms == incoming.observed_at_unix_ms;
    let prefer_previous = same_refresh && previous.archived && !incoming.archived;
    let mut merged = if prefer_previous {
        previous.clone()
    } else {
        incoming
    };
    merged.turns_complete = previous.turns_complete;
    merged.items_complete = previous.items_complete;
    merged.content_complete = previous.content_complete;
    merged.observed_at_unix_ms = previous.observed_at_unix_ms.max(merged.observed_at_unix_ms);
    merged.missing_from_source = false;
    if same_refresh {
        merged.archived |= previous.archived;
    }
    merged
}

impl SessionStore {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, AppError> {
        fs::create_dir_all(&app_data_dir)
            .map_err(|_| AppError::store("创建应用数据目录失败，请检查目录权限。"))?;
        let store = Self {
            path: app_data_dir.join("sessions.sqlite3"),
        };
        let mut connection = store.connection()?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| AppError::store("读取会话数据库版本失败。"))?;
        if version > 4 {
            return Err(AppError::migration(
                "会话数据库来自更新版本的应用，请使用相应版本打开。",
            ));
        }
        if version == 0 {
            let transaction = connection
                .transaction()
                .map_err(|_| AppError::migration("开始会话数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS threads (
                    id TEXT PRIMARY KEY NOT NULL,
                    metadata_json TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS list_scopes (
                    archived INTEGER PRIMARY KEY NOT NULL,
                    complete INTEGER NOT NULL,
                    attempted_at_unix_ms INTEGER,
                    completed_at_unix_ms INTEGER,
                    error TEXT
                );
                PRAGMA user_version = 1;",
                )
                .map_err(|_| AppError::migration("迁移会话数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交会话数据库迁移失败，原数据已保留。"))?;
        }
        if version < 2 {
            let transaction = connection
                .transaction()
                .map_err(|_| AppError::migration("开始项目数据库迁移失败。"))?;
            transaction.execute_batch(
                "CREATE TABLE projects (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_json TEXT NOT NULL
                );
                CREATE TABLE thread_attributions (
                    thread_id TEXT PRIMARY KEY NOT NULL,
                    project_id TEXT,
                    attribution_json TEXT NOT NULL
                );
                CREATE INDEX thread_attributions_project ON thread_attributions(project_id);
                CREATE TABLE project_selection (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    selected_project_id TEXT,
                    recent_json TEXT NOT NULL
                );
                INSERT INTO project_selection (id, selected_project_id, recent_json) VALUES (1, NULL, '[]');
                PRAGMA user_version = 2;",
            ).map_err(|_| AppError::migration("迁移项目数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交项目数据库迁移失败，原数据已保留。"))?;
        }
        if version < 3 {
            let transaction = connection
                .transaction()
                .map_err(|_| AppError::migration("开始索引运行与观察关系数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS index_runs (
                    id TEXT PRIMARY KEY NOT NULL,
                    run_json TEXT NOT NULL,
                    started_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS index_runs_started ON index_runs(started_at DESC);
                CREATE TABLE IF NOT EXISTS observed_relations (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_id TEXT NOT NULL,
                    relation_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS observed_relations_project ON observed_relations(project_id);
                PRAGMA user_version = 3;",
                )
                .map_err(|_| AppError::migration("迁移索引运行与观察关系数据库失败，原数据已保留。"))?;
            transaction.commit().map_err(|_| {
                AppError::migration("提交索引运行与观察关系数据库迁移失败，原数据已保留。")
            })?;
        }
        if version < 4 {
            let transaction = connection
                .transaction()
                .map_err(|_| AppError::migration("开始合并数据库结构迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS index_runs (
                        id TEXT PRIMARY KEY NOT NULL,
                        run_json TEXT NOT NULL,
                        started_at INTEGER NOT NULL
                    );
                    CREATE INDEX IF NOT EXISTS index_runs_started ON index_runs(started_at DESC);
                    CREATE TABLE IF NOT EXISTS observed_relations (
                        id TEXT PRIMARY KEY NOT NULL,
                        project_id TEXT NOT NULL,
                        relation_json TEXT NOT NULL
                    );
                    CREATE INDEX IF NOT EXISTS observed_relations_project ON observed_relations(project_id);
                    PRAGMA user_version = 4;",
                )
                .map_err(|_| AppError::migration("合并数据库结构迁移失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交合并数据库结构迁移失败，原数据已保留。"))?;
        }
        store.recover_interrupted_runs()?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, AppError> {
        Connection::open(&self.path)
            .map_err(|_| AppError::store("打开会话数据库失败，请检查应用数据目录。"))
    }

    pub fn begin_refresh(&self, attempted_at_unix_ms: i64) -> Result<(), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始记录会话刷新状态失败。"))?;
        for archived in [false, true] {
            transaction.execute(
                "INSERT INTO list_scopes (archived, complete, attempted_at_unix_ms, completed_at_unix_ms, error)
                 VALUES (?1, 0, ?2, NULL, '刷新尚未完成；若应用已重启，上次刷新已中断。')
                 ON CONFLICT(archived) DO UPDATE SET
                   complete=0,
                   attempted_at_unix_ms=excluded.attempted_at_unix_ms,
                   error=excluded.error",
                params![archived, attempted_at_unix_ms],
            ).map_err(|_| AppError::store("记录会话刷新状态失败，旧缓存已保留。"))?;
        }
        transaction
            .commit()
            .map_err(|_| AppError::store("提交会话刷新状态失败，旧缓存已保留。"))
    }

    pub fn fail_refresh(&self, attempted_at_unix_ms: i64, message: &str) -> Result<(), AppError> {
        self.connection()?
            .execute(
                "UPDATE list_scopes SET complete=0, error=?1 WHERE attempted_at_unix_ms=?2",
                params![message, attempted_at_unix_ms],
            )
            .map_err(|_| AppError::store("记录会话刷新失败状态失败，旧缓存已保留。"))?;
        Ok(())
    }

    pub fn save_collection(
        &self,
        threads: &[ThreadMetadata],
        scopes: &[ListScopeStatus],
    ) -> Result<(), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存会话列表失败。"))?;
        for thread in threads {
            let mut thread = thread.clone();
            let previous: Option<String> = transaction
                .query_row(
                    "SELECT metadata_json FROM threads WHERE id=?1",
                    params![thread.id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("读取已有会话完整性失败。"))?;
            if let Some(previous) = previous {
                let previous: ThreadMetadata = serde_json::from_str(&previous)
                    .map_err(|_| AppError::store("已有会话元数据损坏，旧缓存已保留。"))?;
                thread = merge_thread_metadata(previous, thread);
            }
            let json = serde_json::to_string(&thread)
                .map_err(|_| AppError::store("序列化会话元数据失败。"))?;
            transaction.execute(
                "INSERT INTO threads (id, metadata_json, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET metadata_json=excluded.metadata_json, updated_at=excluded.updated_at",
                params![thread.id, json, thread.updated_at],
            ).map_err(|_| AppError::store("保存会话元数据失败，旧缓存已保留。"))?;
        }
        for scope in scopes {
            transaction.execute(
                "INSERT INTO list_scopes (archived, complete, attempted_at_unix_ms, completed_at_unix_ms, error)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(archived) DO UPDATE SET
                   complete=excluded.complete,
                   attempted_at_unix_ms=excluded.attempted_at_unix_ms,
                   completed_at_unix_ms=COALESCE(excluded.completed_at_unix_ms, list_scopes.completed_at_unix_ms),
                   error=excluded.error",
                params![scope.archived, scope.complete, scope.attempted_at_unix_ms, scope.completed_at_unix_ms, scope.error],
            ).map_err(|_| AppError::store("保存列表完整性失败，旧缓存已保留。"))?;
            if scope.complete {
                let mut query = transaction
                    .prepare("SELECT id, metadata_json FROM threads")
                    .map_err(|_| AppError::store("检查来源完整列表失败。"))?;
                let rows = query
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(|_| AppError::store("检查来源完整列表失败。"))?;
                let mut stale = Vec::new();
                for row in rows {
                    let (id, json) = row.map_err(|_| AppError::store("读取来源完整列表失败。"))?;
                    let mut thread: ThreadMetadata = serde_json::from_str(&json)
                        .map_err(|_| AppError::store("会话缓存内容损坏，无法确认来源完整性。"))?;
                    if thread.archived == scope.archived
                        && thread.observed_at_unix_ms
                            < scope.attempted_at_unix_ms.unwrap_or_default()
                        && !thread.missing_from_source
                    {
                        thread.missing_from_source = true;
                        stale.push((
                            id,
                            serde_json::to_string(&thread)
                                .map_err(|_| AppError::store("序列化来源完整性失败。"))?,
                        ));
                    }
                }
                drop(query);
                for (id, json) in stale {
                    transaction
                        .execute(
                            "UPDATE threads SET metadata_json=?1 WHERE id=?2",
                            params![json, id],
                        )
                        .map_err(|_| AppError::store("保存来源完整性失败，旧缓存已保留。"))?;
                }
            }
        }
        transaction
            .commit()
            .map_err(|_| AppError::store("提交会话列表失败，旧缓存已保留。"))
    }

    pub fn save_index_run(&self, run: &IndexRun) -> Result<(), AppError> {
        let json =
            serde_json::to_string(run).map_err(|_| AppError::store("序列化索引运行失败。"))?;
        self.connection()?
            .execute(
                "INSERT INTO index_runs (id, run_json, started_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET run_json=excluded.run_json",
                params![run.id, json, run.started_at_unix_ms],
            )
            .map_err(|_| AppError::store("保存索引运行失败，旧缓存已保留。"))?;
        Ok(())
    }

    pub fn index_run(&self, id: &str) -> Result<Option<IndexRun>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_json FROM index_runs WHERE id=?1")
            .map_err(|_| AppError::store("读取索引运行失败。"))?;
        let mut rows = query
            .query(params![id])
            .map_err(|_| AppError::store("查询索引运行失败。"))?;
        match rows
            .next()
            .map_err(|_| AppError::store("读取索引运行失败。"))?
        {
            Some(row) => {
                let json: String = row
                    .get(0)
                    .map_err(|_| AppError::store("读取索引运行失败。"))?;
                Ok(Some(
                    serde_json::from_str(&json)
                        .map_err(|_| AppError::store("索引运行数据损坏。"))?,
                ))
            }
            None => Ok(None),
        }
    }

    pub fn latest_index_run(&self) -> Result<Option<IndexRun>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_json FROM index_runs ORDER BY started_at DESC, id DESC LIMIT 1")
            .map_err(|_| AppError::store("读取最近索引运行失败。"))?;
        let mut rows = query
            .query([])
            .map_err(|_| AppError::store("查询最近索引运行失败。"))?;
        match rows
            .next()
            .map_err(|_| AppError::store("读取最近索引运行失败。"))?
        {
            Some(row) => {
                let json: String = row
                    .get(0)
                    .map_err(|_| AppError::store("读取最近索引运行失败。"))?;
                Ok(Some(
                    serde_json::from_str(&json)
                        .map_err(|_| AppError::store("索引运行数据损坏。"))?,
                ))
            }
            None => Ok(None),
        }
    }

    fn recover_interrupted_runs(&self) -> Result<(), AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_json FROM index_runs")
            .map_err(|_| AppError::store("读取待恢复索引运行失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询待恢复索引运行失败。"))?;
        let mut interrupted = Vec::new();
        for row in rows {
            let json = row.map_err(|_| AppError::store("读取待恢复索引运行失败。"))?;
            let mut run: IndexRun =
                serde_json::from_str(&json).map_err(|_| AppError::store("索引运行数据损坏。"))?;
            if !run.state.terminal() {
                run.state = IndexRunState::Partial;
                run.interrupted = true;
                run.finished_at_unix_ms = Some(now_ms() as i64);
                run.error = Some(AppError::codex(
                    codexflow_domain::ErrorCode::SourceReadFailed,
                    "上次索引运行因应用退出而中断；已提交缓存保留，可重新刷新。",
                    true,
                ));
                interrupted.push(run);
            }
        }
        drop(query);
        for run in interrupted {
            self.save_index_run(&run)?;
        }
        Ok(())
    }

    pub fn list(&self) -> Result<SessionList, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT metadata_json FROM threads ORDER BY updated_at DESC, id")
            .map_err(|_| AppError::store("读取会话缓存失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询会话缓存失败。"))?;
        let mut threads = Vec::new();
        for row in rows {
            let json = row.map_err(|_| AppError::store("读取会话缓存失败。"))?;
            threads.push(
                serde_json::from_str(&json)
                    .map_err(|_| AppError::store("会话缓存内容损坏，无法继续读取。"))?,
            );
        }
        Ok(SessionList {
            threads,
            scopes: self.scopes(&connection)?,
        })
    }

    fn scopes(&self, connection: &Connection) -> Result<Vec<ListScopeStatus>, AppError> {
        let mut query = connection.prepare("SELECT archived, complete, attempted_at_unix_ms, completed_at_unix_ms, error FROM list_scopes ORDER BY archived")
            .map_err(|_| AppError::store("读取列表完整性失败。"))?;
        let rows = query
            .query_map([], |row| {
                Ok(ListScopeStatus {
                    archived: row.get(0)?,
                    complete: row.get(1)?,
                    attempted_at_unix_ms: row.get(2)?,
                    completed_at_unix_ms: row.get(3)?,
                    error: row.get(4)?,
                })
            })
            .map_err(|_| AppError::store("查询列表完整性失败。"))?;
        let mut scopes = Vec::new();
        for row in rows {
            scopes.push(row.map_err(|_| AppError::store("读取列表完整性失败。"))?);
        }
        for archived in [false, true] {
            if !scopes.iter().any(|scope| scope.archived == archived) {
                scopes.push(ListScopeStatus {
                    archived,
                    complete: false,
                    attempted_at_unix_ms: None,
                    completed_at_unix_ms: None,
                    error: None,
                });
            }
        }
        scopes.sort_by_key(|scope| scope.archived);
        Ok(scopes)
    }

    pub fn projects(&self) -> Result<Vec<LocalProject>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT project_json FROM projects ORDER BY id")
            .map_err(|_| AppError::store("读取项目列表失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询项目列表失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取项目失败。"))?)
                .map_err(|_| AppError::store("项目缓存内容损坏。"))
        })
        .collect()
    }

    pub fn attributions(&self) -> Result<Vec<ThreadAttribution>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT attribution_json FROM thread_attributions")
            .map_err(|_| AppError::store("读取归属依据失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询归属依据失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取归属依据失败。"))?)
                .map_err(|_| AppError::store("归属依据缓存内容损坏。"))
        })
        .collect()
    }

    pub fn save_projects_and_attributions(
        &self,
        projects: &[LocalProject],
        attributions: &[ThreadAttribution],
        relations: Option<&[ObservedRelation]>,
    ) -> Result<(), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存项目归属失败。"))?;
        for project in projects {
            let json =
                serde_json::to_string(project).map_err(|_| AppError::store("序列化项目失败。"))?;
            transaction
                .execute(
                    "INSERT INTO projects (id, project_json) VALUES (?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET project_json=excluded.project_json",
                    params![project.id, json],
                )
                .map_err(|_| AppError::store("保存项目失败。"))?;
        }
        for attribution in attributions {
            let json = serde_json::to_string(attribution)
                .map_err(|_| AppError::store("序列化归属依据失败。"))?;
            transaction.execute(
                "INSERT INTO thread_attributions (thread_id, project_id, attribution_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(thread_id) DO UPDATE SET project_id=excluded.project_id, attribution_json=excluded.attribution_json",
                params![attribution.thread_id, attribution.project_id, json],
            ).map_err(|_| AppError::store("保存归属依据失败。"))?;
        }
        if let Some(relations) = relations {
            transaction
                .execute("DELETE FROM observed_relations", [])
                .map_err(|_| AppError::store("更新观察关系失败，旧数据已保留。"))?;
            for relation in relations {
                let json = serde_json::to_string(relation)
                    .map_err(|_| AppError::store("序列化观察关系失败。"))?;
                transaction
                    .execute(
                        "INSERT INTO observed_relations (id, project_id, relation_json) VALUES (?1, ?2, ?3)",
                        params![relation.id, relation.project_id, json],
                    )
                    .map_err(|_| AppError::store("保存观察关系失败，旧数据已保留。"))?;
            }
        }
        transaction
            .commit()
            .map_err(|_| AppError::store("提交项目归属失败，旧数据已保留。"))
    }

    pub fn observed_relations(&self, project_id: &str) -> Result<Vec<ObservedRelation>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT relation_json FROM observed_relations WHERE project_id=?1 ORDER BY id")
            .map_err(|_| AppError::store("读取观察关系失败。"))?;
        let rows = query
            .query_map([project_id], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询观察关系失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取观察关系失败。"))?)
                .map_err(|_| AppError::store("观察关系缓存内容损坏。"))
        })
        .collect()
    }

    pub fn selection(&self) -> Result<(Option<String>, Vec<String>), AppError> {
        let connection = self.connection()?;
        let (selected, recent_json): (Option<String>, String) = connection
            .query_row(
                "SELECT selected_project_id, recent_json FROM project_selection WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| AppError::store("读取最近项目失败。"))?;
        let recent = serde_json::from_str(&recent_json)
            .map_err(|_| AppError::store("最近项目缓存内容损坏。"))?;
        Ok((selected, recent))
    }

    pub fn select_project(&self, project_id: &str) -> Result<(), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存项目选择失败。"))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [project_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("确认项目失败。"))?;
        if !exists {
            return Err(AppError::store("所选项目不存在。"));
        }
        let recent_json: String = transaction
            .query_row(
                "SELECT recent_json FROM project_selection WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("读取最近项目失败。"))?;
        let mut recent: Vec<String> = serde_json::from_str(&recent_json)
            .map_err(|_| AppError::store("最近项目缓存内容损坏。"))?;
        recent.retain(|id| id != project_id);
        recent.insert(0, project_id.to_owned());
        recent.truncate(10);
        let json =
            serde_json::to_string(&recent).map_err(|_| AppError::store("序列化最近项目失败。"))?;
        transaction
            .execute(
                "UPDATE project_selection SET selected_project_id=?1, recent_json=?2 WHERE id=1",
                params![project_id, json],
            )
            .map_err(|_| AppError::store("保存项目选择失败。"))?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交项目选择失败。"))
    }

    pub fn catalog(&self) -> Result<ProjectCatalog, AppError> {
        let connection = self.connection()?;
        let (selected_project_id, recent_project_ids) = self.selection()?;
        let mut query = connection
            .prepare(
                "SELECT t.metadata_json, a.attribution_json FROM threads t
             JOIN thread_attributions a ON a.thread_id=t.id
             WHERE a.project_id IS NULL ORDER BY t.updated_at DESC, t.id",
            )
            .map_err(|_| AppError::store("读取未归属会话失败。"))?;
        let rows = query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| AppError::store("查询未归属会话失败。"))?;
        let unassigned = rows
            .map(|row| {
                let (thread, attribution) =
                    row.map_err(|_| AppError::store("读取未归属会话失败。"))?;
                Ok(AttributedThread {
                    thread: serde_json::from_str(&thread)
                        .map_err(|_| AppError::store("会话缓存内容损坏。"))?,
                    attribution: serde_json::from_str(&attribution)
                        .map_err(|_| AppError::store("归属依据缓存内容损坏。"))?,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        Ok(ProjectCatalog {
            projects: self.projects()?,
            selected_project_id,
            recent_project_ids,
            unassigned,
            scopes: self.scopes(&connection)?,
        })
    }

    pub fn project_sessions(&self, project_id: &str) -> Result<ProjectSessions, AppError> {
        let connection = self.connection()?;
        let project_json: String = connection
            .query_row(
                "SELECT project_json FROM projects WHERE id=?1",
                [project_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("所选项目不存在。"))?;
        let project = serde_json::from_str(&project_json)
            .map_err(|_| AppError::store("项目缓存内容损坏。"))?;
        let mut query = connection
            .prepare(
                "SELECT t.metadata_json, a.attribution_json FROM threads t
             JOIN thread_attributions a ON a.thread_id=t.id
             WHERE a.project_id=?1 ORDER BY t.updated_at DESC, t.id",
            )
            .map_err(|_| AppError::store("读取项目会话失败。"))?;
        let rows = query
            .query_map([project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| AppError::store("查询项目会话失败。"))?;
        let threads: Vec<AttributedThread> = rows
            .map(|row| {
                let (thread, attribution) =
                    row.map_err(|_| AppError::store("读取项目会话失败。"))?;
                Ok(AttributedThread {
                    thread: serde_json::from_str(&thread)
                        .map_err(|_| AppError::store("会话缓存内容损坏。"))?,
                    attribution: serde_json::from_str(&attribution)
                        .map_err(|_| AppError::store("归属依据缓存内容损坏。"))?,
                })
            })
            .collect::<Result<_, AppError>>()?;
        let mut workspaces: Vec<String> = threads
            .iter()
            .filter_map(|item| item.attribution.workspace_root.clone())
            .collect();
        workspaces.sort();
        workspaces.dedup();
        Ok(ProjectSessions {
            project,
            workspaces,
            threads,
            scopes: self.scopes(&connection)?,
        })
    }
}

impl PreferenceStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            path: app_data_dir.join("preferences.json"),
        }
    }

    pub fn load(&self) -> Result<Preferences, AppError> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                AppError::store("显示设置文件无法读取，请检查应用数据目录中的 preferences.json。")
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
            Err(_) => Err(AppError::store(
                "读取显示设置失败，请检查应用数据目录权限。",
            )),
        }
    }

    pub fn save(&self, preferences: &Preferences) -> Result<(), AppError> {
        let parent = self.path.parent().expect("preferences path has parent");
        fs::create_dir_all(parent)
            .map_err(|_| AppError::store("创建应用数据目录失败，请检查目录权限。"))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!("preferences.{nonce}.tmp"));
        let bytes =
            serde_json::to_vec(preferences).map_err(|_| AppError::store("保存显示设置失败。"))?;
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|_| AppError::store("保存显示设置失败，请检查应用数据目录权限。"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{DisplayTheme, ErrorCode};

    fn thread(
        title: &str,
        updated_at: i64,
        archived: bool,
        observed_at_unix_ms: i64,
    ) -> ThreadMetadata {
        ThreadMetadata {
            id: "duplicate-thread".into(),
            session_id: "session-duplicate-thread".into(),
            title: Some(title.into()),
            preview: title.into(),
            cwd: "/tmp/example".into(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at,
            archived,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms,
        }
    }

    #[test]
    fn selection_and_theme_survive_reopen() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-store-test-{nonce}"));
        let store = PreferenceStore::new(dir.clone());
        store
            .save(&Preferences {
                selected_binary: Some("/tmp/codex".into()),
                theme: DisplayTheme::Dark,
                jev: Default::default(),
            })
            .unwrap();
        let loaded = PreferenceStore::new(dir.clone()).load().unwrap();
        assert_eq!(loaded.selected_binary.as_deref(), Some("/tmp/codex"));
        assert!(matches!(loaded.theme, DisplayTheme::Dark));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn newer_database_is_not_replaced_with_an_empty_list() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-new-db-test-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 5;")
            .unwrap();
        drop(connection);
        let error = SessionStore::new(dir.clone())
            .err()
            .expect("migration error");
        assert!(matches!(error.code, ErrorCode::MigrationFailed));
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn interrupted_refresh_remains_incomplete_after_reopen() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-interrupted-test-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        let complete = [false, true].map(|archived| ListScopeStatus {
            archived,
            complete: true,
            attempted_at_unix_ms: Some(1000),
            completed_at_unix_ms: Some(1000),
            error: None,
        });
        store.save_collection(&[], &complete).unwrap();
        store.begin_refresh(2000).unwrap();
        let reopened = SessionStore::new(dir.clone()).unwrap().list().unwrap();
        assert!(reopened.scopes.iter().all(|scope| {
            !scope.complete
                && scope.attempted_at_unix_ms == Some(2000)
                && scope.completed_at_unix_ms == Some(1000)
                && scope
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("中断"))
        }));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn project_selection_recent_history_and_attribution_survive_reopen() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-project-store-test-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        let project = LocalProject {
            id: "dir:/tmp/example".into(),
            name: "example".into(),
            root: "/tmp/example".into(),
            git_common_dir: None,
        };
        let other_project = LocalProject {
            id: "dir:/tmp/other".into(),
            name: "other".into(),
            root: "/tmp/other".into(),
            git_common_dir: None,
        };
        let thread = ThreadMetadata {
            id: "thread-one".into(),
            session_id: "session-one".into(),
            title: None,
            preview: String::new(),
            cwd: "/tmp/example".into(),
            project_id: Some("native-one".into()),
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at: 2,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 3,
        };
        let attribution = ThreadAttribution {
            thread_id: thread.id.clone(),
            project_id: Some(project.id.clone()),
            workspace_root: Some(project.root.clone()),
            basis: "directoryRoot".into(),
            detail: "已验证目录".into(),
            diagnostic: None,
            source_project_id: thread.project_id.clone(),
        };
        store.save_collection(&[thread], &[]).unwrap();
        store
            .save_projects_and_attributions(
                &[project.clone(), other_project.clone()],
                &[attribution],
                None,
            )
            .unwrap();
        store.select_project(&project.id).unwrap();
        store.select_project(&other_project.id).unwrap();
        store.select_project(&project.id).unwrap();
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let catalog = reopened.catalog().unwrap();
        assert_eq!(
            catalog.selected_project_id.as_deref(),
            Some(project.id.as_str())
        );
        assert_eq!(
            catalog.recent_project_ids,
            vec![project.id.clone(), other_project.id]
        );
        let sessions = reopened.project_sessions(&project.id).unwrap();
        assert_eq!(sessions.threads.len(), 1);
        assert_eq!(sessions.threads[0].attribution.detail, "已验证目录");
        assert_eq!(sessions.workspaces, vec![project.root]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_one_session_cache_migrates_without_losing_threads() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-project-migration-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        connection.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY NOT NULL, metadata_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE list_scopes (archived INTEGER PRIMARY KEY NOT NULL, complete INTEGER NOT NULL,
               attempted_at_unix_ms INTEGER, completed_at_unix_ms INTEGER, error TEXT);
             PRAGMA user_version = 1;",
        ).unwrap();
        connection
            .execute(
                "INSERT INTO threads (id, metadata_json, updated_at) VALUES ('legacy', '{}', 1)",
                [],
            )
            .unwrap();
        drop(connection);
        let store = SessionStore::new(dir.clone()).unwrap();
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        let (version, count): (i64, i64) = (
            connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap(),
            connection
                .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
                .unwrap(),
        );
        assert_eq!((version, count), (4, 1));
        assert!(store.latest_index_run().unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_two_project_cache_gains_observed_relations_without_losing_projects() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-relations-migration-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        connection.execute_batch(
            r#"CREATE TABLE threads (id TEXT PRIMARY KEY NOT NULL, metadata_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE list_scopes (archived INTEGER PRIMARY KEY NOT NULL, complete INTEGER NOT NULL,
                attempted_at_unix_ms INTEGER, completed_at_unix_ms INTEGER, error TEXT);
             CREATE TABLE projects (id TEXT PRIMARY KEY NOT NULL, project_json TEXT NOT NULL);
             CREATE TABLE thread_attributions (thread_id TEXT PRIMARY KEY NOT NULL, project_id TEXT, attribution_json TEXT NOT NULL);
             CREATE TABLE project_selection (id INTEGER PRIMARY KEY CHECK (id = 1), selected_project_id TEXT, recent_json TEXT NOT NULL);
             INSERT INTO project_selection (id, selected_project_id, recent_json) VALUES (1, NULL, '[]');
             INSERT INTO projects (id, project_json) VALUES ('existing', '{"id":"existing","name":"现有项目","root":"/tmp/existing","gitCommonDir":null}');
             PRAGMA user_version = 2;"#,
        ).unwrap();
        drop(connection);
        let store = SessionStore::new(dir.clone()).unwrap();
        assert_eq!(store.projects().unwrap().len(), 1);
        assert!(store.observed_relations("existing").unwrap().is_empty());
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        assert!(store.latest_index_run().unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn either_version_three_schema_migrates_without_losing_existing_data() {
        use codexflow_domain::{ObservedRelationKind, ParentEndpoint};

        for has_relations in [false, true] {
            let dir = std::env::temp_dir().join(format!(
                "codexflow-v3-schema-{}-{}-{has_relations}",
                std::process::id(),
                now_ms()
            ));
            fs::create_dir_all(&dir).unwrap();
            let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE threads (id TEXT PRIMARY KEY NOT NULL, metadata_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
                     CREATE TABLE list_scopes (archived INTEGER PRIMARY KEY NOT NULL, complete INTEGER NOT NULL,
                         attempted_at_unix_ms INTEGER, completed_at_unix_ms INTEGER, error TEXT);
                     CREATE TABLE projects (id TEXT PRIMARY KEY NOT NULL, project_json TEXT NOT NULL);
                     CREATE TABLE thread_attributions (thread_id TEXT PRIMARY KEY NOT NULL, project_id TEXT, attribution_json TEXT NOT NULL);
                     CREATE TABLE project_selection (id INTEGER PRIMARY KEY CHECK (id = 1), selected_project_id TEXT, recent_json TEXT NOT NULL);
                     INSERT INTO project_selection (id, selected_project_id, recent_json) VALUES (1, NULL, '[]');",
                )
                .unwrap();
            if has_relations {
                let relation = ObservedRelation {
                    id: "existing-relation".into(),
                    project_id: "existing-project".into(),
                    from_thread_id: "parent".into(),
                    to_thread_id: "child".into(),
                    kind: ObservedRelationKind::SubagentOf,
                    source: "observed".into(),
                    source_field: "parentThreadId".into(),
                    confidence: 1.0,
                    parent_endpoint: ParentEndpoint::InProject,
                };
                connection
                    .execute_batch(
                        "CREATE TABLE observed_relations (id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL, relation_json TEXT NOT NULL);
                         CREATE INDEX observed_relations_project ON observed_relations(project_id);",
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO observed_relations (id, project_id, relation_json) VALUES (?1, ?2, ?3)",
                        params![relation.id, relation.project_id, serde_json::to_string(&relation).unwrap()],
                    )
                    .unwrap();
            } else {
                let run = IndexRun {
                    id: "existing-run".into(),
                    project_id: Some("existing-project".into()),
                    state: IndexRunState::Complete,
                    started_at_unix_ms: 10,
                    finished_at_unix_ms: Some(20),
                    pages_saved: 1,
                    threads_seen: 1,
                    error: None,
                    interrupted: false,
                };
                connection
                    .execute_batch(
                        "CREATE TABLE index_runs (id TEXT PRIMARY KEY NOT NULL, run_json TEXT NOT NULL, started_at INTEGER NOT NULL);
                         CREATE INDEX index_runs_started ON index_runs(started_at DESC);",
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO index_runs (id, run_json, started_at) VALUES (?1, ?2, ?3)",
                        params![
                            run.id,
                            serde_json::to_string(&run).unwrap(),
                            run.started_at_unix_ms
                        ],
                    )
                    .unwrap();
            }
            connection
                .execute_batch("PRAGMA user_version = 3;")
                .unwrap();
            drop(connection);

            let store = SessionStore::new(dir.clone()).unwrap();
            assert_eq!(
                store.observed_relations("existing-project").unwrap().len(),
                usize::from(has_relations)
            );
            assert_eq!(
                store
                    .latest_index_run()
                    .unwrap()
                    .as_ref()
                    .map(|run| run.id.as_str()),
                (!has_relations).then_some("existing-run")
            );
            let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 4);
            let tables: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('index_runs', 'observed_relations')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(tables, 2);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn reopening_marks_unfinished_run_interrupted_without_erasing_cache() {
        let nonce = now_ms();
        let dir = std::env::temp_dir().join(format!(
            "codexflow-index-reopen-{}-{nonce}",
            std::process::id()
        ));
        let store = SessionStore::new(dir.clone()).unwrap();
        let run = IndexRun {
            id: "run-one".into(),
            project_id: Some("project-one".into()),
            state: IndexRunState::Running,
            started_at_unix_ms: 123,
            finished_at_unix_ms: None,
            pages_saved: 2,
            threads_seen: 12,
            error: None,
            interrupted: false,
        };
        store.save_index_run(&run).unwrap();
        drop(store);
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let recovered = reopened.index_run("run-one").unwrap().unwrap();
        assert_eq!(recovered.state, IndexRunState::Partial);
        assert!(recovered.interrupted);
        assert_eq!(recovered.pages_saved, 2);
        assert_eq!(recovered.project_id.as_deref(), Some("project-one"));
        assert!(recovered.error.unwrap().cache_preserved);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn only_complete_scope_marks_old_threads_absent_and_metadata_keeps_content_quality() {
        let dir = std::env::temp_dir().join(format!(
            "codexflow-presence-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = SessionStore::new(dir.clone()).unwrap();
        let mut thread = ThreadMetadata {
            id: "thread-a".into(),
            session_id: "session-a".into(),
            title: None,
            preview: String::new(),
            cwd: "/tmp/example".into(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at: 2,
            archived: false,
            metadata_complete: true,
            turns_complete: true,
            items_complete: true,
            missing_from_source: false,
            content_complete: true,
            read_error: None,
            observed_at_unix_ms: 100,
        };
        store.save_collection(&[thread.clone()], &[]).unwrap();
        thread.turns_complete = false;
        thread.items_complete = false;
        thread.content_complete = false;
        thread.observed_at_unix_ms = 200;
        store.save_collection(&[thread], &[]).unwrap();
        let cached = &store.list().unwrap().threads[0];
        assert!(cached.turns_complete && cached.items_complete && cached.content_complete);

        let partial = ListScopeStatus {
            archived: false,
            complete: false,
            attempted_at_unix_ms: Some(300),
            completed_at_unix_ms: None,
            error: Some("中断".into()),
        };
        store.save_collection(&[], &[partial]).unwrap();
        assert!(!store.list().unwrap().threads[0].missing_from_source);
        let complete = ListScopeStatus {
            archived: false,
            complete: true,
            attempted_at_unix_ms: Some(301),
            completed_at_unix_ms: Some(301),
            error: None,
        };
        store.save_collection(&[], &[complete]).unwrap();
        assert!(store.list().unwrap().threads[0].missing_from_source);
        assert_eq!(store.list().unwrap().threads.len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn older_duplicate_does_not_replace_newer_metadata_or_content_integrity() {
        let dir = std::env::temp_dir().join(format!(
            "codexflow-newest-duplicate-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = SessionStore::new(dir.clone()).unwrap();
        let mut newest = thread("newest metadata", 20, false, 100);
        newest.turns_complete = true;
        newest.items_complete = true;
        newest.content_complete = true;
        newest.missing_from_source = true;
        store.save_collection(&[newest], &[]).unwrap();

        let stale = thread("stale archived copy", 10, true, 200);
        store.save_collection(&[stale], &[]).unwrap();

        let cached = store.list().unwrap().threads.remove(0);
        assert_eq!(cached.updated_at, 20);
        assert_eq!(cached.title.as_deref(), Some("newest metadata"));
        assert!(!cached.archived);
        assert!(cached.turns_complete && cached.items_complete && cached.content_complete);
        assert!(!cached.missing_from_source);
        assert_eq!(cached.observed_at_unix_ms, 200);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn same_version_archive_duplicate_has_order_independent_metadata_and_integrity() {
        for (index, archive_first) in [false, true].into_iter().enumerate() {
            let dir = std::env::temp_dir().join(format!(
                "codexflow-archive-duplicate-{}-{}-{index}",
                std::process::id(),
                now_ms()
            ));
            let store = SessionStore::new(dir.clone()).unwrap();
            let mut cached = thread("cached metadata", 20, false, 100);
            cached.turns_complete = true;
            cached.items_complete = true;
            cached.content_complete = true;
            store.save_collection(&[cached], &[]).unwrap();

            let live = thread("live scope copy", 20, false, 200);
            let archived = thread("archived scope copy", 20, true, 200);
            if archive_first {
                store.save_collection(&[archived], &[]).unwrap();
                store.save_collection(&[live], &[]).unwrap();
            } else {
                store.save_collection(&[live], &[]).unwrap();
                store.save_collection(&[archived], &[]).unwrap();
            }

            let cached = store.list().unwrap().threads.remove(0);
            assert_eq!(cached.title.as_deref(), Some("archived scope copy"));
            assert!(cached.archived);
            assert!(cached.turns_complete && cached.items_complete && cached.content_complete);
            assert_eq!(cached.observed_at_unix_ms, 200);
            let _ = fs::remove_dir_all(dir);
        }
    }
}
