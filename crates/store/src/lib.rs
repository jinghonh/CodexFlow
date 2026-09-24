use codexflow_domain::{
    AnalysisRun, AnalysisRunState, AnalysisUnitState, AppError, AttributedThread, CandidatePreview,
    DerivedRelation, EvidencePage, FactPage, HistoryCoverage, HistoryItem, HistoryItemLocation,
    HistoryItemPage, HistorySnapshot, HistoryTurn, HistoryTurnPage, IndexRun, IndexRunState,
    InferredPairOutcome, InferredRelation, ListScopeStatus, LocalProject, ObservedRelation,
    Preferences, ProjectCatalog, ProjectSessions, RelationReview, SessionList, SourceEvidence,
    SourceFact, SummaryRun, SummaryRunState, ThreadAttribution, ThreadMetadata, ThreadSummary,
    UserRelationDecision, Workstream,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct PreferenceStore {
    database_path: PathBuf,
    legacy_path: PathBuf,
}

const SESSION_SCHEMA_VERSION: i64 = 15;

pub struct SessionStore {
    path: PathBuf,
}

/// Human corrections are stored apart from the replaceable automatic groups.
pub struct WorkstreamCorrections {
    pub revision: u64,
    pub names: BTreeMap<String, String>,
    /// None means explicitly ungrouped; an absent key means use automatic ownership.
    pub members: BTreeMap<String, Option<String>>,
}

pub enum WorkstreamChange<'a> {
    Rename {
        id: &'a str,
        name: &'a str,
    },
    RestoreName {
        id: &'a str,
    },
    Move {
        thread_id: &'a str,
        target_id: Option<&'a str>,
    },
    RestoreMember {
        thread_id: &'a str,
    },
}

fn bump_workstream_revision(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> Result<(), AppError> {
    transaction
        .execute(
            "INSERT INTO workstream_revisions(project_id,revision) VALUES (?1,1)
         ON CONFLICT(project_id) DO UPDATE SET revision=revision+1",
            [project_id],
        )
        .map_err(|_| AppError::store("更新工作流修订号失败。"))?;
    Ok(())
}

pub struct ProjectMaterial {
    pub facts: Vec<SourceFact>,
    pub evidence: Vec<SourceEvidence>,
    pub items: Vec<HistoryItem>,
}

/// One read transaction supplies every row used to validate one evidence pointer.
pub struct EvidenceSourceSnapshot {
    pub evidence: SourceEvidence,
    pub thread: Option<ThreadMetadata>,
    pub turn_exists: bool,
    pub item: Option<HistoryItem>,
    pub item_in_other_turn: bool,
    pub fact: Option<SourceFact>,
    pub location: Option<HistoryItemLocation>,
}

fn locate_history_item_in(
    connection: &Connection,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
) -> Result<Option<HistoryItemLocation>, AppError> {
    let mut location = connection.query_row(
        "SELECT i.turn_id, (SELECT COUNT(*) FROM history_items p WHERE p.thread_id=i.thread_id AND p.turn_id=i.turn_id
            AND (p.ordinal<i.ordinal OR (p.ordinal=i.ordinal AND p.id<i.id)))
         FROM history_items i WHERE i.thread_id=?1 AND i.turn_id=?2 AND i.id=?3",
        params![thread_id, turn_id, item_id], |row| Ok(HistoryItemLocation { turn_id: row.get(0)?, turn_offset: 0, offset: row.get(1)? }),
    ).optional().map_err(|_| AppError::store("定位条目失败。"))?;
    if let Some(found) = &mut location {
        found.turn_offset = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM history_turns p WHERE p.thread_id=t.thread_id
                AND (p.ordinal<t.ordinal OR (p.ordinal=t.ordinal AND p.id<t.id)))
             FROM history_turns t WHERE t.thread_id=?1 AND t.id=?2",
                params![thread_id, found.turn_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("定位回合失败。"))?
            .unwrap_or(0);
    }
    Ok(location)
}

fn complete_history_at_revision(
    connection: &Connection,
    thread_id: &str,
    source_updated_at: i64,
    generation: i64,
    fact_rule: Option<&str>,
) -> Result<bool, AppError> {
    let indexed_at: Option<i64> = connection
        .query_row(
            "SELECT updated_at FROM threads WHERE id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| AppError::store("检查会话来源版本失败。"))?;
    let coverage: Option<String> = connection
        .query_row(
            "SELECT coverage_json FROM history_coverage WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| AppError::store("检查历史完整性失败。"))?;
    let coverage = coverage
        .map(|json| {
            serde_json::from_str::<HistoryCoverage>(&json)
                .map_err(|_| AppError::store("历史覆盖范围损坏。"))
        })
        .transpose()?;
    let current_generation: Option<i64> = connection
        .query_row(
            "SELECT generation FROM history_revisions WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| AppError::store("检查历史代次失败。"))?;
    let source_current = indexed_at == Some(source_updated_at)
        && current_generation.unwrap_or(0) == generation
        && coverage.is_some_and(|coverage| {
            coverage.source_updated_at == source_updated_at
                && coverage.turns_complete
                && coverage.items_complete
        });
    if !source_current {
        return Ok(false);
    }
    if let Some(rule) = fact_rule {
        let fact_generation: Option<i64> = connection
            .query_row(
                "SELECT history_generation FROM fact_index WHERE thread_id=?1 AND rule_version=?2",
                params![thread_id, rule],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查事实提取版本失败。"))?;
        return Ok(fact_generation == Some(generation));
    }
    Ok(true)
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
    if merged.read_error.is_none() {
        merged.read_error = previous.read_error.clone();
    }
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
        if version > SESSION_SCHEMA_VERSION {
            return Err(AppError::database_too_new());
        }
        if version == SESSION_SCHEMA_VERSION {
            store.recover_interrupted_runs()?;
            store.recover_interrupted_summary_runs()?;
            store.recover_interrupted_analysis_runs()?;
            return Ok(store);
        }
        let mut migration = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::migration("开始会话数据库升级失败，原数据已保留。"))?;
        if version == 0 {
            let transaction = migration
                .savepoint()
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
            let transaction = migration
                .savepoint()
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
            let transaction = migration
                .savepoint()
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
            let transaction = migration
                .savepoint()
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
        if version < 5 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始历史数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE history_coverage (
                    thread_id TEXT PRIMARY KEY NOT NULL,
                    coverage_json TEXT NOT NULL
                );
                CREATE TABLE history_turns (
                    thread_id TEXT NOT NULL,
                    id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    turn_json TEXT NOT NULL,
                    PRIMARY KEY (thread_id, id)
                );
                CREATE INDEX history_turn_order ON history_turns(thread_id, ordinal, id);
                CREATE TABLE history_items (
                    thread_id TEXT NOT NULL,
                    id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    item_json TEXT NOT NULL,
                    PRIMARY KEY (thread_id, id)
                );
                CREATE INDEX history_item_order ON history_items(thread_id, turn_id, ordinal, id);
                PRAGMA user_version = 5;",
                )
                .map_err(|_| AppError::migration("迁移历史数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交历史数据库迁移失败，原数据已保留。"))?;
        }
        if version < 6 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始条目身份数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE history_items_v6 (
                        thread_id TEXT NOT NULL,
                        turn_id TEXT NOT NULL,
                        id TEXT NOT NULL,
                        ordinal INTEGER NOT NULL,
                        item_json TEXT NOT NULL,
                        PRIMARY KEY (thread_id, turn_id, id)
                    );
                    INSERT INTO history_items_v6 (thread_id, turn_id, id, ordinal, item_json)
                        SELECT thread_id, turn_id, id, ordinal, item_json FROM history_items;
                    DROP TABLE history_items;
                    ALTER TABLE history_items_v6 RENAME TO history_items;
                    CREATE INDEX history_item_order ON history_items(thread_id, turn_id, ordinal, id);
                    PRAGMA user_version = 6;",
                )
                .map_err(|_| AppError::migration("迁移条目身份数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交条目身份数据库迁移失败，原数据已保留。"))?;
        }
        if version < 7 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始事实与证据数据库迁移失败。"))?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS source_facts (
                    id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL, fact_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS source_facts_thread ON source_facts(thread_id, ordinal, id);
                CREATE TABLE IF NOT EXISTS source_evidence (
                    id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL, evidence_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS source_evidence_thread ON source_evidence(thread_id, ordinal, id);
                CREATE TABLE IF NOT EXISTS fact_index (
                    thread_id TEXT PRIMARY KEY NOT NULL, content_digest TEXT NOT NULL,
                    rule_version TEXT NOT NULL, history_generation INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS history_revisions (
                    thread_id TEXT PRIMARY KEY NOT NULL, generation INTEGER NOT NULL
                );
                PRAGMA user_version = 7;",
            ).map_err(|_| AppError::migration("迁移事实与证据数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交事实与证据数据库迁移失败，原数据已保留。"))?;
        }
        if version < 8 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始规则关系数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS automatic_candidate_views (
                    project_id TEXT PRIMARY KEY NOT NULL,
                    preview_json TEXT NOT NULL,
                    relations_json TEXT NOT NULL
                );
                PRAGMA user_version = 8;",
                )
                .map_err(|_| AppError::migration("迁移规则关系数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交规则关系数据库迁移失败，原数据已保留。"))?;
        }
        // The issue branches independently used schema version 8. Version 9 fills
        // in either missing layout so databases from both branches upgrade safely.
        if version < 9 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始总结与规则关系数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS automatic_candidate_views (
                    project_id TEXT PRIMARY KEY NOT NULL,
                    preview_json TEXT NOT NULL,
                    relations_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS thread_summaries (
                    thread_id TEXT PRIMARY KEY NOT NULL, summary_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS summary_runs (
                    id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL,
                    started_at INTEGER NOT NULL, run_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS summary_runs_thread ON summary_runs(thread_id, started_at DESC);
                PRAGMA user_version = 9;",
                )
                .map_err(|_| AppError::migration("迁移总结与规则关系数据库失败，原数据已保留。"))?;
            transaction.commit().map_err(|_| {
                AppError::migration("提交总结与规则关系数据库迁移失败，原数据已保留。")
            })?;
        }
        if version < 10 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始分析运行数据库迁移失败。"))?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS analysis_runs (
                    id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL,
                    started_at INTEGER NOT NULL, run_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS analysis_runs_project ON analysis_runs(project_id, started_at DESC);
                PRAGMA user_version = 10;"
            ).map_err(|_| AppError::migration("迁移分析运行数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交分析运行数据库迁移失败，原数据已保留。"))?;
        }
        if version < 11 {
            migration
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS inferred_pair_outcomes (
                candidate_id TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL,
                result_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS inferred_pair_project ON inferred_pair_outcomes(project_id);
            PRAGMA user_version = 11;",
                )
                .map_err(|_| AppError::migration("迁移推断关系数据库失败，原数据已保留。"))?;
        }
        if version < 12 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始关系裁决数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS relation_reviews (
                    relation_id TEXT PRIMARY KEY NOT NULL,
                    project_id TEXT NOT NULL,
                    decision TEXT NOT NULL CHECK(decision IN ('pending','confirmed','rejected')),
                    revision INTEGER NOT NULL,
                    confirmed_evidence_version TEXT,
                    relation_json TEXT
                );
                CREATE INDEX IF NOT EXISTS relation_reviews_project ON relation_reviews(project_id);
                PRAGMA user_version = 12;",
                )
                .map_err(|_| AppError::migration("迁移关系裁决数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交关系裁决数据库迁移失败，原数据已保留。"))?;
        }
        if version < 13 {
            migration
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS workstreams (
                    id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL,
                    stream_json TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS workstreams_project ON workstreams(project_id);
                PRAGMA user_version = 13;",
                )
                .map_err(|_| AppError::migration("迁移工作流数据库失败，原数据已保留。"))?;
        }
        if version < 14 {
            let transaction = migration
                .savepoint()
                .map_err(|_| AppError::migration("开始工作流修正数据库迁移失败。"))?;
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS workstream_revisions (
                    project_id TEXT PRIMARY KEY NOT NULL, revision INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS workstream_name_corrections (
                    project_id TEXT NOT NULL, workstream_id TEXT NOT NULL, name TEXT NOT NULL,
                    PRIMARY KEY(project_id,workstream_id)
                );
                CREATE TABLE IF NOT EXISTS workstream_member_corrections (
                    project_id TEXT NOT NULL, thread_id TEXT NOT NULL, workstream_id TEXT,
                    PRIMARY KEY(project_id,thread_id)
                );
                CREATE INDEX IF NOT EXISTS workstream_member_targets
                    ON workstream_member_corrections(project_id,workstream_id);
                PRAGMA user_version = 14;",
                )
                .map_err(|_| AppError::migration("迁移工作流修正数据库失败，原数据已保留。"))?;
            transaction
                .commit()
                .map_err(|_| AppError::migration("提交工作流修正数据库迁移失败，原数据已保留。"))?;
        }
        if version < 15 {
            migration
                .execute_batch(
                    "CREATE TABLE app_preferences (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    preferences_json TEXT NOT NULL
                );
                PRAGMA user_version = 15;",
                )
                .map_err(|_| AppError::migration("迁移应用设置数据库失败，原数据已保留。"))?;
        }
        migration
            .commit()
            .map_err(|_| AppError::migration("提交会话数据库升级失败，原数据已保留。"))?;
        store.recover_interrupted_runs()?;
        store.recover_interrupted_summary_runs()?;
        store.recover_interrupted_analysis_runs()?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, AppError> {
        let connection = Connection::open(&self.path)
            .map_err(|_| AppError::store("打开会话数据库失败，请检查应用数据目录。"))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| AppError::store("设置会话数据库等待时间失败。"))?;
        Ok(connection)
    }

    pub fn workstreams(&self, project_id: &str) -> Result<Vec<Workstream>, AppError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT stream_json FROM workstreams WHERE project_id=?1 ORDER BY id")
            .map_err(|_| AppError::store("读取工作流失败。"))?;
        let streams = statement
            .query_map([project_id], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("读取工作流失败。"))?
            .map(|row| {
                let json = row.map_err(|_| AppError::store("读取工作流失败。"))?;
                serde_json::from_str(&json).map_err(|_| AppError::store("解析工作流失败。"))
            })
            .collect();
        streams
    }

    pub fn workstream_corrections(
        &self,
        project_id: &str,
    ) -> Result<WorkstreamCorrections, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("读取工作流修正失败。"))?;
        let corrections = Self::workstream_corrections_in(&transaction, project_id)?;
        transaction
            .commit()
            .map_err(|_| AppError::store("完成读取工作流修正失败。"))?;
        Ok(corrections)
    }

    /// Read the automatic groups and corrections from one SQLite snapshot.
    pub fn workstream_snapshot(
        &self,
        project_id: &str,
    ) -> Result<(Vec<Workstream>, WorkstreamCorrections), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("读取工作流快照失败。"))?;
        let streams = {
            let mut statement = transaction
                .prepare("SELECT stream_json FROM workstreams WHERE project_id=?1 ORDER BY id")
                .map_err(|_| AppError::store("读取工作流失败。"))?;
            let rows = statement
                .query_map([project_id], |row| row.get::<_, String>(0))
                .map_err(|_| AppError::store("读取工作流失败。"))?;
            rows.map(|row| {
                let json = row.map_err(|_| AppError::store("读取工作流失败。"))?;
                serde_json::from_str(&json).map_err(|_| AppError::store("解析工作流失败。"))
            })
            .collect::<Result<Vec<_>, _>>()?
        };
        let corrections = Self::workstream_corrections_in(&transaction, project_id)?;
        transaction
            .commit()
            .map_err(|_| AppError::store("完成读取工作流快照失败。"))?;
        Ok((streams, corrections))
    }

    fn workstream_corrections_in(
        transaction: &rusqlite::Transaction<'_>,
        project_id: &str,
    ) -> Result<WorkstreamCorrections, AppError> {
        let revision: i64 = transaction
            .query_row(
                "SELECT revision FROM workstream_revisions WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流修订号失败。"))?
            .unwrap_or(0);
        let names = {
            let mut query = transaction.prepare(
                "SELECT workstream_id,name FROM workstream_name_corrections WHERE project_id=?1"
            ).map_err(|_| AppError::store("读取工作流名称修正失败。"))?;
            let entries = query
                .query_map([project_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|_| AppError::store("读取工作流名称修正失败。"))?
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map_err(|_| AppError::store("读取工作流名称修正失败。"))?;
            entries
        };
        let members = {
            let mut query = transaction.prepare(
                "SELECT thread_id,workstream_id FROM workstream_member_corrections WHERE project_id=?1"
            ).map_err(|_| AppError::store("读取会话归属修正失败。"))?;
            let entries = query
                .query_map([project_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|_| AppError::store("读取会话归属修正失败。"))?
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map_err(|_| AppError::store("读取会话归属修正失败。"))?;
            entries
        };
        Ok(WorkstreamCorrections {
            revision: revision as u64,
            names,
            members,
        })
    }

    pub fn save_workstream_correction(
        &self,
        project_id: &str,
        expected_revision: u64,
        change: WorkstreamChange<'_>,
    ) -> Result<u64, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存工作流修正失败。"))?;
        let current: i64 = transaction
            .query_row(
                "SELECT revision FROM workstream_revisions WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流修订号失败。"))?
            .unwrap_or(0);
        if current != i64::try_from(expected_revision).unwrap_or(-1) {
            return Err(AppError::workstream_conflict());
        }
        match change {
            WorkstreamChange::Rename { id, name } => {
                let exists: bool = transaction
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM workstreams WHERE id=?1 AND project_id=?2)",
                        params![id, project_id],
                        |row| row.get(0),
                    )
                    .map_err(|_| AppError::store("检查工作流失败。"))?;
                if !exists {
                    return Err(AppError::workstream_conflict());
                }
                transaction.execute(
                    "INSERT INTO workstream_name_corrections(project_id,workstream_id,name) VALUES (?1,?2,?3)
                     ON CONFLICT(project_id,workstream_id) DO UPDATE SET name=excluded.name",
                    params![project_id,id,name]
                ).map_err(|_| AppError::store("保存工作流名称修正失败。"))?;
            }
            WorkstreamChange::RestoreName { id } => {
                transaction.execute(
                    "DELETE FROM workstream_name_corrections WHERE project_id=?1 AND workstream_id=?2",
                    params![project_id,id]
                ).map_err(|_| AppError::store("恢复自动工作流名称失败。"))?;
            }
            WorkstreamChange::Move {
                thread_id,
                target_id,
            } => {
                if let Some(target_id) = target_id {
                    let exists: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM workstreams WHERE id=?1 AND project_id=?2)",
                        params![target_id, project_id], |row| row.get(0)
                    ).map_err(|_| AppError::store("检查目标工作流失败。"))?;
                    if !exists {
                        return Err(AppError::workstream_conflict());
                    }
                }
                transaction.execute(
                    "INSERT INTO workstream_member_corrections(project_id,thread_id,workstream_id) VALUES (?1,?2,?3)
                     ON CONFLICT(project_id,thread_id) DO UPDATE SET workstream_id=excluded.workstream_id",
                    params![project_id,thread_id,target_id]
                ).map_err(|_| AppError::store("保存会话归属修正失败。"))?;
            }
            WorkstreamChange::RestoreMember { thread_id } => {
                transaction.execute(
                    "DELETE FROM workstream_member_corrections WHERE project_id=?1 AND thread_id=?2",
                    params![project_id,thread_id]
                ).map_err(|_| AppError::store("恢复自动会话归属失败。"))?;
            }
        }
        bump_workstream_revision(&transaction, project_id)?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交工作流修正失败。"))?;
        Ok(expected_revision.saturating_add(1))
    }

    pub fn replace_workstreams(
        &self,
        project_id: &str,
        expected_revision: u64,
        streams: &[Workstream],
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存工作流失败。"))?;
        let current: i64 = transaction
            .query_row(
                "SELECT revision FROM workstream_revisions WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流修订号失败。"))?
            .unwrap_or(0);
        if current != i64::try_from(expected_revision).unwrap_or(-1) {
            return Ok(false);
        }
        transaction
            .execute("DELETE FROM workstreams WHERE project_id=?1", [project_id])
            .map_err(|_| AppError::store("清理旧工作流失败。"))?;
        for stream in streams {
            if stream.project_id != project_id {
                return Err(AppError::store("工作流所属项目不一致。"));
            }
            let json =
                serde_json::to_string(stream).map_err(|_| AppError::store("序列化工作流失败。"))?;
            transaction
                .execute(
                    "INSERT INTO workstreams(id,project_id,stream_json) VALUES (?1,?2,?3)",
                    params![stream.id, project_id, json],
                )
                .map_err(|_| AppError::store("保存工作流失败。"))?;
        }
        bump_workstream_revision(&transaction, project_id)?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交工作流失败。"))?;
        Ok(true)
    }

    pub fn save_workstream_name(
        &self,
        project_id: &str,
        id: &str,
        members: &[String],
        name: &str,
        input_version: &str,
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存工作流名称失败。"))?;
        let json: Option<String> = transaction
            .query_row(
                "SELECT stream_json FROM workstreams WHERE id=?1 AND project_id=?2",
                params![id, project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流名称失败。"))?;
        let Some(json) = json else { return Ok(false) };
        let mut stream: Workstream =
            serde_json::from_str(&json).map_err(|_| AppError::store("解析工作流失败。"))?;
        if stream.members != members {
            return Ok(false);
        }
        stream.name = name.into();
        stream.name_input_version = Some(input_version.into());
        stream.name_actual_model = None;
        stream.name_error = None;
        let json =
            serde_json::to_string(&stream).map_err(|_| AppError::store("序列化工作流失败。"))?;
        transaction
            .execute(
                "UPDATE workstreams SET stream_json=?1 WHERE id=?2 AND project_id=?3",
                params![json, id, project_id],
            )
            .map_err(|_| AppError::store("保存工作流名称失败。"))?;
        bump_workstream_revision(&transaction, project_id)?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交工作流名称失败。"))?;
        Ok(true)
    }

    pub fn save_workstream_name_error(
        &self,
        project_id: &str,
        id: &str,
        members: &[String],
        message: &str,
    ) -> Result<(), AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存命名错误失败。"))?;
        let json: Option<String> = transaction
            .query_row(
                "SELECT stream_json FROM workstreams WHERE id=?1 AND project_id=?2",
                params![id, project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流失败。"))?;
        if let Some(json) = json {
            let mut stream: Workstream =
                serde_json::from_str(&json).map_err(|_| AppError::store("解析工作流失败。"))?;
            if stream.members == members && stream.name_input_version.is_none() {
                stream.name_error = Some(message.into());
                let json = serde_json::to_string(&stream)
                    .map_err(|_| AppError::store("序列化工作流失败。"))?;
                transaction
                    .execute(
                        "UPDATE workstreams SET stream_json=?1 WHERE id=?2",
                        params![json, id],
                    )
                    .map_err(|_| AppError::store("保存命名错误失败。"))?;
                bump_workstream_revision(&transaction, project_id)?;
            }
        }
        transaction
            .commit()
            .map_err(|_| AppError::store("提交命名错误失败。"))
    }

    pub fn save_analysis_with_workstream_name(
        &self,
        run: &AnalysisRun,
        id: &str,
        members: &[String],
        name: &str,
        input_version: &str,
        actual_model: &str,
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存命名结果失败。"))?;
        let current: String = transaction
            .query_row(
                "SELECT run_json FROM analysis_runs WHERE id=?1",
                [&run.id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("读取分析运行失败。"))?;
        let current: AnalysisRun =
            serde_json::from_str(&current).map_err(|_| AppError::store("解析分析运行失败。"))?;
        if current.state == AnalysisRunState::Cancelling {
            return Ok(false);
        }
        let stream: Option<String> = transaction
            .query_row(
                "SELECT stream_json FROM workstreams WHERE id=?1 AND project_id=?2",
                params![id, run.project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取工作流失败。"))?;
        let Some(stream) = stream else {
            return Ok(false);
        };
        let mut stream: Workstream =
            serde_json::from_str(&stream).map_err(|_| AppError::store("解析工作流失败。"))?;
        if stream.members != members {
            return Ok(false);
        }
        stream.name = name.into();
        stream.name_input_version = Some(input_version.into());
        stream.name_actual_model = Some(actual_model.into());
        stream.name_error = None;
        let stream_json =
            serde_json::to_string(&stream).map_err(|_| AppError::store("序列化工作流失败。"))?;
        let run_json =
            serde_json::to_string(run).map_err(|_| AppError::store("序列化分析运行失败。"))?;
        transaction
            .execute(
                "UPDATE workstreams SET stream_json=?1 WHERE id=?2",
                params![stream_json, id],
            )
            .map_err(|_| AppError::store("保存工作流名称失败。"))?;
        bump_workstream_revision(&transaction, &run.project_id)?;
        transaction
            .execute(
                "UPDATE analysis_runs SET run_json=?1 WHERE id=?2",
                params![run_json, run.id],
            )
            .map_err(|_| AppError::store("保存分析运行失败。"))?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交命名结果失败。"))?;
        Ok(true)
    }

    pub fn save_analysis_run(&self, run: &AnalysisRun) -> Result<(), AppError> {
        let json =
            serde_json::to_string(run).map_err(|_| AppError::store("序列化分析运行失败。"))?;
        self.connection()?
            .execute(
                "INSERT INTO analysis_runs(id,project_id,started_at,run_json) VALUES (?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET run_json=excluded.run_json",
                params![run.id, run.project_id, run.started_at_unix_ms, json],
            )
            .map_err(|_| AppError::store("保存分析运行失败。"))?;
        Ok(())
    }

    pub fn analysis_run(&self, id: &str) -> Result<Option<AnalysisRun>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT run_json FROM analysis_runs WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取分析运行失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("分析运行状态损坏。"))
        })
        .transpose()
    }

    pub fn thread_project_id(&self, thread_id: &str) -> Result<Option<String>, AppError> {
        self.connection()?
            .query_row(
                "SELECT project_id FROM thread_attributions WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(|_| AppError::store("读取会话项目归属失败。"))
    }

    pub fn latest_analysis_run(&self, project_id: &str) -> Result<Option<AnalysisRun>, AppError> {
        let json: Option<String> = self.connection()?.query_row(
            "SELECT run_json FROM analysis_runs WHERE project_id=?1 ORDER BY started_at DESC,id DESC LIMIT 1",
            [project_id], |row| row.get(0)
        ).optional().map_err(|_| AppError::store("读取项目分析运行失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("分析运行状态损坏。"))
        })
        .transpose()
    }

    fn recover_interrupted_analysis_runs(&self) -> Result<(), AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_json FROM analysis_runs")
            .map_err(|_| AppError::store("读取待恢复分析运行失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询待恢复分析运行失败。"))?;
        let mut interrupted = Vec::new();
        for row in rows {
            let json = row.map_err(|_| AppError::store("读取待恢复分析运行失败。"))?;
            let mut run: AnalysisRun =
                serde_json::from_str(&json).map_err(|_| AppError::store("分析运行状态损坏。"))?;
            if matches!(
                run.state,
                AnalysisRunState::Queued | AnalysisRunState::Running | AnalysisRunState::Cancelling
            ) {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some("上次运行因应用退出而中断，可继续未完成单元。".into());
                run.interrupted = true;
                run.finished_at_unix_ms = Some(now_ms() as i64);
                for unit in &mut run.units {
                    if unit.state == AnalysisUnitState::Running {
                        unit.state = AnalysisUnitState::Pending;
                        unit.active_summary_run_id = None;
                    }
                }
                interrupted.push(run);
            }
        }
        drop(query);
        drop(connection);
        for run in interrupted {
            self.save_analysis_run(&run)?;
        }
        Ok(())
    }

    pub fn summary(&self, thread_id: &str) -> Result<Option<ThreadSummary>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT summary_json FROM thread_summaries WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取会话总结失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("会话总结缓存损坏。"))
        })
        .transpose()
    }

    pub fn save_summary_if_current(
        &self,
        summary: &ThreadSummary,
        expected_generation: i64,
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存会话总结失败。"))?;
        let metadata: Option<String> = tx
            .query_row(
                "SELECT metadata_json FROM threads WHERE id=?1",
                [&summary.thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查会话来源版本失败。"))?;
        let generation: Option<i64> = tx
            .query_row(
                "SELECT generation FROM history_revisions WHERE thread_id=?1",
                [&summary.thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查历史版本失败。"))?;
        let current = metadata
            .and_then(|value| serde_json::from_str::<ThreadMetadata>(&value).ok())
            .is_some_and(|thread| thread.updated_at == summary.source_updated_at)
            && generation.unwrap_or(0) == expected_generation;
        if !current {
            return Ok(false);
        }
        let json =
            serde_json::to_string(summary).map_err(|_| AppError::store("序列化会话总结失败。"))?;
        tx.execute(
            "INSERT INTO thread_summaries(thread_id,summary_json) VALUES (?1,?2)
            ON CONFLICT(thread_id) DO UPDATE SET summary_json=excluded.summary_json",
            params![summary.thread_id, json],
        )
        .map_err(|_| AppError::store("保存会话总结失败，旧总结已保留。"))?;
        tx.commit()
            .map_err(|_| AppError::store("提交会话总结失败，旧总结已保留。"))?;
        Ok(true)
    }

    pub fn save_summary_run(&self, run: &SummaryRun) -> Result<(), AppError> {
        let json =
            serde_json::to_string(run).map_err(|_| AppError::store("序列化总结运行失败。"))?;
        self.connection()?
            .execute(
                "INSERT INTO summary_runs(id,thread_id,started_at,run_json) VALUES (?1,?2,?3,?4)
            ON CONFLICT(id) DO UPDATE SET run_json=excluded.run_json",
                params![run.id, run.thread_id, run.started_at_unix_ms, json],
            )
            .map_err(|_| AppError::store("保存总结运行状态失败。"))?;
        Ok(())
    }

    pub fn summary_run(&self, id: &str) -> Result<Option<SummaryRun>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT run_json FROM summary_runs WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取总结运行状态失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("总结运行状态损坏。"))
        })
        .transpose()
    }

    pub fn latest_summary_run(&self, thread_id: &str) -> Result<Option<SummaryRun>, AppError> {
        let json: Option<String> = self.connection()?.query_row(
            "SELECT run_json FROM summary_runs WHERE thread_id=?1 ORDER BY started_at DESC,id DESC LIMIT 1", [thread_id], |row| row.get(0)
        ).optional().map_err(|_| AppError::store("读取最近总结运行失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("总结运行状态损坏。"))
        })
        .transpose()
    }

    fn recover_interrupted_summary_runs(&self) -> Result<(), AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT run_json FROM summary_runs")
            .map_err(|_| AppError::store("读取待恢复总结运行失败。"))?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询待恢复总结运行失败。"))?;
        let mut interrupted = Vec::new();
        for row in rows {
            let json = row.map_err(|_| AppError::store("读取待恢复总结运行失败。"))?;
            let mut run: SummaryRun =
                serde_json::from_str(&json).map_err(|_| AppError::store("总结运行状态损坏。"))?;
            if matches!(
                run.state,
                SummaryRunState::Running | SummaryRunState::Cancelling
            ) {
                run.state = SummaryRunState::Failed;
                run.finished_at_unix_ms = Some(now_ms() as i64);
                run.error = Some(AppError::codex(
                    codexflow_domain::ErrorCode::AnalysisUnavailable,
                    "上次总结运行因应用退出而中断；旧总结已保留。",
                    true,
                ));
                interrupted.push(run);
            }
        }
        drop(query);
        drop(connection);
        for run in interrupted {
            self.save_summary_run(&run)?;
        }
        Ok(())
    }

    pub fn thread(&self, thread_id: &str) -> Result<Option<ThreadMetadata>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT metadata_json FROM threads WHERE id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取会话元数据失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("会话元数据损坏。"))
        })
        .transpose()
    }

    pub fn save_history(&self, snapshot: &HistorySnapshot) -> Result<HistoryCoverage, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存会话历史失败。"))?;
        let coverage = snapshot.coverage.clone();
        let indexed_version: Option<i64> = tx
            .query_row(
                "SELECT updated_at FROM threads WHERE id=?1",
                [&coverage.thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查会话来源版本失败。"))?;
        if indexed_version != Some(coverage.source_updated_at) {
            return Err(AppError::store(
                "会话来源在读取期间已变化；迟到历史未保存，请重新读取。",
            ));
        }
        let previous: Option<String> = tx
            .query_row(
                "SELECT coverage_json FROM history_coverage WHERE thread_id=?1",
                [&coverage.thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取已有历史覆盖范围失败。"))?;
        let mut content_changed = previous.is_none();
        if let Some(previous) = previous {
            let mut previous: HistoryCoverage = serde_json::from_str(&previous)
                .map_err(|_| AppError::store("已有历史覆盖范围损坏。"))?;
            if previous.source_updated_at > coverage.source_updated_at {
                return Ok(previous);
            }
            if !coverage.items_complete
                && previous.source_updated_at == coverage.source_updated_at
                && previous.items_complete
            {
                previous.attempted_at_unix_ms = coverage.attempted_at_unix_ms;
                previous.error = coverage.error;
                let json = serde_json::to_string(&previous)
                    .map_err(|_| AppError::store("序列化历史覆盖范围失败。"))?;
                tx.execute(
                    "UPDATE history_coverage SET coverage_json=?1 WHERE thread_id=?2",
                    params![json, previous.thread_id],
                )
                .map_err(|_| AppError::store("记录最近读取状态失败。"))?;
                let metadata_json: String = tx
                    .query_row(
                        "SELECT metadata_json FROM threads WHERE id=?1",
                        [&previous.thread_id],
                        |row| row.get(0),
                    )
                    .map_err(|_| AppError::store("读取会话元数据失败。"))?;
                let mut metadata: ThreadMetadata = serde_json::from_str(&metadata_json)
                    .map_err(|_| AppError::store("会话元数据损坏。"))?;
                metadata.read_error = previous.error.clone();
                tx.execute(
                    "UPDATE threads SET metadata_json=?1 WHERE id=?2",
                    params![
                        serde_json::to_string(&metadata)
                            .map_err(|_| AppError::store("序列化会话元数据失败。"))?,
                        previous.thread_id
                    ],
                )
                .map_err(|_| AppError::store("记录最近读取错误失败。"))?;
                tx.commit()
                    .map_err(|_| AppError::store("提交最近读取状态失败。"))?;
                return Ok(previous);
            }
            content_changed |= previous.source_updated_at != coverage.source_updated_at
                || previous.turns_complete != coverage.turns_complete
                || previous.items_complete != coverage.items_complete;
        }
        if snapshot.coverage.items_complete {
            let old_turns: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM history_turns WHERE thread_id=?1",
                    [&coverage.thread_id],
                    |row| row.get(0),
                )
                .map_err(|_| AppError::store("检查回合缓存失败。"))?;
            let old_items: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM history_items WHERE thread_id=?1",
                    [&coverage.thread_id],
                    |row| row.get(0),
                )
                .map_err(|_| AppError::store("检查条目缓存失败。"))?;
            content_changed |= old_turns != snapshot.turns.len() as i64
                || old_items != snapshot.items.len() as i64;
            for turn in &snapshot.turns {
                let prior: Option<String> = tx
                    .query_row(
                        "SELECT turn_json FROM history_turns WHERE thread_id=?1 AND id=?2",
                        params![turn.thread_id, turn.id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|_| AppError::store("检查回合内容失败。"))?;
                content_changed |= prior.as_deref()
                    != Some(
                        serde_json::to_string(turn)
                            .map_err(|_| AppError::store("序列化回合失败。"))?
                            .as_str(),
                    );
            }
            for item in &snapshot.items {
                let prior: Option<String> = tx.query_row(
                    "SELECT item_json FROM history_items WHERE thread_id=?1 AND turn_id=?2 AND id=?3",
                    params![item.thread_id, item.turn_id, item.id], |row| row.get(0),
                ).optional().map_err(|_| AppError::store("检查条目内容失败。"))?;
                content_changed |= prior.as_deref()
                    != Some(
                        serde_json::to_string(item)
                            .map_err(|_| AppError::store("序列化条目失败。"))?
                            .as_str(),
                    );
            }
        }
        if snapshot.coverage.items_complete {
            tx.execute(
                "DELETE FROM history_items WHERE thread_id=?1",
                [&coverage.thread_id],
            )
            .map_err(|_| AppError::store("替换完整条目缓存失败。"))?;
            tx.execute(
                "DELETE FROM history_turns WHERE thread_id=?1",
                [&coverage.thread_id],
            )
            .map_err(|_| AppError::store("替换完整回合缓存失败。"))?;
        }
        for turn in &snapshot.turns {
            let json =
                serde_json::to_string(turn).map_err(|_| AppError::store("序列化回合失败。"))?;
            if !snapshot.coverage.items_complete {
                let prior: Option<String> = tx
                    .query_row(
                        "SELECT turn_json FROM history_turns WHERE thread_id=?1 AND id=?2",
                        params![turn.thread_id, turn.id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|_| AppError::store("检查回合内容失败。"))?;
                content_changed |= prior.as_deref() != Some(json.as_str());
            }
            tx.execute("INSERT INTO history_turns (thread_id,id,ordinal,turn_json) VALUES (?1,?2,?3,?4)
                ON CONFLICT(thread_id,id) DO UPDATE SET ordinal=excluded.ordinal,turn_json=excluded.turn_json",
                params![turn.thread_id, turn.id, turn.ordinal, json],
            ).map_err(|_| AppError::store("保存回合失败，旧缓存已保留。"))?;
        }
        for item in &snapshot.items {
            let json =
                serde_json::to_string(item).map_err(|_| AppError::store("序列化条目失败。"))?;
            if !snapshot.coverage.items_complete {
                let prior: Option<String> = tx.query_row(
                    "SELECT item_json FROM history_items WHERE thread_id=?1 AND turn_id=?2 AND id=?3",
                    params![item.thread_id, item.turn_id, item.id], |row| row.get(0),
                ).optional().map_err(|_| AppError::store("检查条目内容失败。"))?;
                content_changed |= prior.as_deref() != Some(json.as_str());
            }
            tx.execute("INSERT INTO history_items (thread_id,turn_id,id,ordinal,item_json) VALUES (?1,?2,?3,?4,?5)
                ON CONFLICT(thread_id,turn_id,id) DO UPDATE SET ordinal=excluded.ordinal,item_json=excluded.item_json",
                params![item.thread_id, item.turn_id, item.id, item.ordinal, json],
            ).map_err(|_| AppError::store("保存条目失败，旧缓存已保留。"))?;
        }
        let json = serde_json::to_string(&coverage)
            .map_err(|_| AppError::store("序列化历史覆盖范围失败。"))?;
        tx.execute(
            "INSERT INTO history_coverage (thread_id,coverage_json) VALUES (?1,?2)
            ON CONFLICT(thread_id) DO UPDATE SET coverage_json=excluded.coverage_json",
            params![coverage.thread_id, json],
        )
        .map_err(|_| AppError::store("保存历史覆盖范围失败。"))?;
        let existing: String = tx
            .query_row(
                "SELECT metadata_json FROM threads WHERE id=?1",
                [&coverage.thread_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("读取会话元数据失败。"))?;
        let mut metadata: ThreadMetadata =
            serde_json::from_str(&existing).map_err(|_| AppError::store("会话元数据损坏。"))?;
        metadata.turns_complete =
            coverage.turns_complete && metadata.updated_at == coverage.source_updated_at;
        metadata.items_complete =
            coverage.items_complete && metadata.updated_at == coverage.source_updated_at;
        metadata.content_complete = metadata.turns_complete && metadata.items_complete;
        metadata.read_error = coverage.error.clone();
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|_| AppError::store("序列化会话元数据失败。"))?;
        tx.execute(
            "UPDATE threads SET metadata_json=?1 WHERE id=?2",
            params![metadata_json, coverage.thread_id],
        )
        .map_err(|_| AppError::store("更新会话完整性失败。"))?;
        if content_changed {
            tx.execute(
                "INSERT INTO history_revisions (thread_id,generation) VALUES (?1,1)
                 ON CONFLICT(thread_id) DO UPDATE SET generation=generation+1",
                [&coverage.thread_id],
            )
            .map_err(|_| AppError::store("更新历史内容版本失败。"))?;
        }
        tx.commit()
            .map_err(|_| AppError::store("提交会话历史失败，旧缓存已保留。"))?;
        Ok(coverage)
    }

    pub fn history_coverage(&self, thread_id: &str) -> Result<Option<HistoryCoverage>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT coverage_json FROM history_coverage WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取历史覆盖范围失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("历史覆盖范围损坏。"))
        })
        .transpose()
    }

    pub fn history_turns(
        &self,
        thread_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<HistoryTurnPage, AppError> {
        let limit = limit.clamp(1, 100);
        let connection = self.connection()?;
        let total: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_turns WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("统计回合失败。"))?;
        let mut query = connection.prepare("SELECT turn_json FROM history_turns WHERE thread_id=?1 ORDER BY ordinal,id LIMIT ?2 OFFSET ?3")
            .map_err(|_| AppError::store("读取回合失败。"))?;
        let rows = query
            .query_map(
                params![thread_id, limit, offset.min(i64::MAX as u64) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| AppError::store("查询回合失败。"))?;
        let turns = rows
            .map(|row| {
                serde_json::from_str(&row.map_err(|_| AppError::store("读取回合失败。"))?)
                    .map_err(|_| AppError::store("回合缓存损坏。"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HistoryTurnPage {
            coverage: self.history_coverage(thread_id)?,
            turns,
            total,
            offset,
            limit,
        })
    }

    pub fn history_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<Option<HistoryTurn>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT turn_json FROM history_turns WHERE thread_id=?1 AND id=?2",
                params![thread_id, turn_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取证据回合失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("证据回合缓存损坏。"))
        })
        .transpose()
    }

    pub fn project_turns(&self, project_id: &str) -> Result<Vec<HistoryTurn>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare(
                "SELECT h.turn_json FROM history_turns h
             JOIN thread_attributions a ON a.thread_id=h.thread_id
             WHERE a.project_id=?1 ORDER BY h.thread_id,h.ordinal,h.id",
            )
            .map_err(|_| AppError::store("读取项目回合失败。"))?;
        let rows = query
            .query_map([project_id], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询项目回合失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取项目回合失败。"))?)
                .map_err(|_| AppError::store("回合缓存损坏。"))
        })
        .collect()
    }

    pub fn history_items(
        &self,
        thread_id: &str,
        turn_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<HistoryItemPage, AppError> {
        let limit = limit.clamp(1, 100);
        let connection = self.connection()?;
        let total: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_items WHERE thread_id=?1 AND turn_id=?2",
                params![thread_id, turn_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("统计条目失败。"))?;
        let mut query = connection.prepare("SELECT item_json FROM history_items WHERE thread_id=?1 AND turn_id=?2 ORDER BY ordinal,id LIMIT ?3 OFFSET ?4")
            .map_err(|_| AppError::store("读取条目失败。"))?;
        let rows = query
            .query_map(
                params![
                    thread_id,
                    turn_id,
                    limit,
                    offset.min(i64::MAX as u64) as i64
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| AppError::store("查询条目失败。"))?;
        let items = rows
            .map(|row| {
                serde_json::from_str(&row.map_err(|_| AppError::store("读取条目失败。"))?)
                    .map_err(|_| AppError::store("条目缓存损坏。"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HistoryItemPage {
            coverage: self.history_coverage(thread_id)?,
            items,
            total,
            offset,
            limit,
        })
    }

    pub fn locate_history_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> Result<Option<HistoryItemLocation>, AppError> {
        locate_history_item_in(&self.connection()?, thread_id, turn_id, item_id)
    }

    pub fn all_history_items(&self, thread_id: &str) -> Result<Vec<HistoryItem>, AppError> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare(
                "SELECT i.item_json FROM history_items i LEFT JOIN history_turns t
            ON t.thread_id=i.thread_id AND t.id=i.turn_id WHERE i.thread_id=?1
            ORDER BY t.ordinal,i.turn_id,i.ordinal,i.id",
            )
            .map_err(|_| AppError::store("读取事实来源条目失败。"))?;
        let rows = query
            .query_map([thread_id], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询事实来源条目失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取事实来源条目失败。"))?)
                .map_err(|_| AppError::store("事实来源条目损坏。"))
        })
        .collect()
    }

    pub fn history_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> Result<Option<HistoryItem>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT item_json FROM history_items WHERE thread_id=?1 AND turn_id=?2 AND id=?3",
                params![thread_id, turn_id, item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取总结证据条目失败。"))?;
        json.map(|value| {
            serde_json::from_str(&value).map_err(|_| AppError::store("总结证据条目损坏。"))
        })
        .transpose()
    }

    pub fn history_generation(&self, thread_id: &str) -> Result<i64, AppError> {
        self.connection()?
            .query_row(
                "SELECT generation FROM history_revisions WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or(0))
            .map_err(|_| AppError::store("读取历史内容版本失败。"))
    }

    pub fn fact_index(&self, thread_id: &str) -> Result<Option<(String, String, i64)>, AppError> {
        self.connection()?
            .query_row(
                "SELECT content_digest,rule_version,history_generation FROM fact_index WHERE thread_id=?1",
                [thread_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| AppError::store("读取事实提取版本失败。"))
    }

    pub fn replace_automatic_facts(
        &self,
        thread_id: &str,
        source_updated_at: i64,
        content_digest: &str,
        rule_version: &str,
        generation: i64,
        facts: &[SourceFact],
        evidence: &[SourceEvidence],
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始更新自动事实失败。"))?;
        if !complete_history_at_revision(&tx, thread_id, source_updated_at, generation, None)? {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM source_evidence WHERE thread_id=?1",
            [thread_id],
        )
        .map_err(|_| AppError::store("替换旧证据失败。"))?;
        tx.execute("DELETE FROM source_facts WHERE thread_id=?1", [thread_id])
            .map_err(|_| AppError::store("替换旧事实失败。"))?;
        for (ordinal, fact) in facts.iter().enumerate() {
            let json =
                serde_json::to_string(fact).map_err(|_| AppError::store("序列化事实失败。"))?;
            tx.execute(
                "INSERT INTO source_facts (id,thread_id,ordinal,fact_json) VALUES (?1,?2,?3,?4)",
                params![fact.id, thread_id, ordinal as i64, json],
            )
            .map_err(|_| AppError::store("保存自动事实失败。"))?;
        }
        for (ordinal, item) in evidence.iter().enumerate() {
            let json =
                serde_json::to_string(item).map_err(|_| AppError::store("序列化证据失败。"))?;
            tx.execute("INSERT INTO source_evidence (id,thread_id,ordinal,evidence_json) VALUES (?1,?2,?3,?4)",
                params![item.id, thread_id, ordinal as i64, json])
                .map_err(|_| AppError::store("保存自动证据失败。"))?;
        }
        tx.execute("INSERT INTO fact_index (thread_id,content_digest,rule_version,history_generation) VALUES (?1,?2,?3,?4)
            ON CONFLICT(thread_id) DO UPDATE SET content_digest=excluded.content_digest,rule_version=excluded.rule_version,history_generation=excluded.history_generation",
            params![thread_id, content_digest, rule_version, generation])
            .map_err(|_| AppError::store("保存事实提取版本失败。"))?;
        tx.commit()
            .map_err(|_| AppError::store("提交自动事实失败。"))?;
        Ok(true)
    }

    pub fn mark_fact_index_current(
        &self,
        thread_id: &str,
        source_updated_at: i64,
        generation: i64,
        content_digest: &str,
        rule_version: &str,
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始更新事实索引失败。"))?;
        if !complete_history_at_revision(&tx, thread_id, source_updated_at, generation, None)? {
            return Ok(false);
        }
        let changed = tx.execute(
            "UPDATE fact_index SET history_generation=?2 WHERE thread_id=?1 AND content_digest=?3 AND rule_version=?4",
            params![thread_id, generation, content_digest, rule_version],
        ).map_err(|_| AppError::store("更新事实索引版本失败。"))?;
        tx.commit()
            .map_err(|_| AppError::store("提交事实索引版本失败。"))?;
        Ok(changed == 1)
    }

    pub fn facts(&self, thread_id: &str, offset: u64, limit: u32) -> Result<FactPage, AppError> {
        let limit = limit.clamp(1, 100);
        let connection = self.connection()?;
        let total: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM source_facts WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("统计事实失败。"))?;
        let mut query = connection.prepare("SELECT fact_json FROM source_facts WHERE thread_id=?1 ORDER BY ordinal,id LIMIT ?2 OFFSET ?3")
            .map_err(|_| AppError::store("读取事实失败。"))?;
        let rows = query
            .query_map(
                params![thread_id, limit, offset.min(i64::MAX as u64) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| AppError::store("查询事实失败。"))?;
        let facts = rows
            .map(|row| {
                serde_json::from_str(&row.map_err(|_| AppError::store("读取事实失败。"))?)
                    .map_err(|_| AppError::store("事实缓存损坏。"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FactPage {
            facts,
            total,
            offset,
            limit,
            coverage: self.history_coverage(thread_id)?,
        })
    }

    pub fn evidence_page(
        &self,
        thread_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<EvidencePage, AppError> {
        let limit = limit.clamp(1, 100);
        let connection = self.connection()?;
        let total: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM source_evidence WHERE thread_id=?1",
                [thread_id],
                |row| row.get(0),
            )
            .map_err(|_| AppError::store("统计证据失败。"))?;
        let mut query = connection.prepare("SELECT evidence_json FROM source_evidence WHERE thread_id=?1 ORDER BY ordinal,id LIMIT ?2 OFFSET ?3")
            .map_err(|_| AppError::store("读取证据失败。"))?;
        let rows = query
            .query_map(
                params![thread_id, limit, offset.min(i64::MAX as u64) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| AppError::store("查询证据失败。"))?;
        let evidence = rows
            .map(|row| {
                serde_json::from_str(&row.map_err(|_| AppError::store("读取证据失败。"))?)
                    .map_err(|_| AppError::store("证据缓存损坏。"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EvidencePage {
            evidence,
            total,
            offset,
            limit,
        })
    }

    pub fn evidence_snapshot(
        &self,
        evidence: SourceEvidence,
    ) -> Result<EvidenceSourceSnapshot, AppError> {
        self.read_evidence_snapshot(Some(evidence), None, || {})?
            .ok_or_else(|| AppError::store("证据快照缺失。"))
    }

    pub fn stored_evidence_snapshot(
        &self,
        evidence_id: &str,
    ) -> Result<Option<EvidenceSourceSnapshot>, AppError> {
        self.read_evidence_snapshot(None, Some(evidence_id), || {})
    }

    fn read_evidence_snapshot(
        &self,
        supplied: Option<SourceEvidence>,
        evidence_id: Option<&str>,
        after_item: impl FnOnce(),
    ) -> Result<Option<EvidenceSourceSnapshot>, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始证据一致性检查失败。"))?;
        let evidence = match supplied {
            Some(evidence) => evidence,
            None => {
                let id = evidence_id.ok_or_else(|| AppError::store("证据标识缺失。"))?;
                let json: Option<String> = tx
                    .query_row(
                        "SELECT evidence_json FROM source_evidence WHERE id=?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|_| AppError::store("读取证据失败。"))?;
                let Some(json) = json else {
                    return Ok(None);
                };
                serde_json::from_str(&json).map_err(|_| AppError::store("证据缓存损坏。"))?
            }
        };
        let thread_json: Option<String> = tx
            .query_row(
                "SELECT metadata_json FROM threads WHERE id=?1",
                [&evidence.thread_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查证据会话失败。"))?;
        let thread = thread_json
            .map(|json| {
                serde_json::from_str(&json).map_err(|_| AppError::store("会话元数据损坏。"))
            })
            .transpose()?;
        let turn_exists = tx
            .query_row(
                "SELECT 1 FROM history_turns WHERE thread_id=?1 AND id=?2",
                params![evidence.thread_id, evidence.turn_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|_| AppError::store("检查证据回合失败。"))?
            .is_some();
        let item_json: Option<String> = tx
            .query_row(
                "SELECT item_json FROM history_items WHERE thread_id=?1 AND turn_id=?2 AND id=?3",
                params![evidence.thread_id, evidence.turn_id, evidence.item_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查证据条目失败。"))?;
        let item = item_json
            .map(|json| serde_json::from_str(&json).map_err(|_| AppError::store("证据条目损坏。")))
            .transpose()?;
        after_item();
        let item_in_other_turn =
            if item.is_none() {
                tx.query_row(
                "SELECT 1 FROM history_items WHERE thread_id=?1 AND turn_id<>?2 AND id=?3 LIMIT 1",
                params![evidence.thread_id, evidence.turn_id, evidence.item_id], |_| Ok(()),
            ).optional().map_err(|_| AppError::store("检查证据层级失败。"))?.is_some()
            } else {
                false
            };
        let fact_json: Option<String> = tx
            .query_row(
                "SELECT fact_json FROM source_facts WHERE id=?1",
                [&evidence.fact_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取证据所属事实失败。"))?;
        let fact = fact_json
            .map(|json| serde_json::from_str(&json).map_err(|_| AppError::store("事实缓存损坏。")))
            .transpose()?;
        let location = if item.is_some() && turn_exists {
            locate_history_item_in(
                &tx,
                &evidence.thread_id,
                &evidence.turn_id,
                &evidence.item_id,
            )?
        } else {
            None
        };
        tx.commit()
            .map_err(|_| AppError::store("完成证据一致性检查失败。"))?;
        Ok(Some(EvidenceSourceSnapshot {
            evidence,
            thread,
            turn_exists,
            item,
            item_in_other_turn,
            fact,
            location,
        }))
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

    pub fn project_material(
        &self,
        project_id: &str,
        expected: &[(String, i64, i64)],
        fact_rule: &str,
    ) -> Result<Option<ProjectMaterial>, AppError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始读取项目来源材料失败。"))?;
        for (thread_id, source_updated_at, generation) in expected {
            let owner: Option<Option<String>> = transaction
                .query_row(
                    "SELECT project_id FROM thread_attributions WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选项目归属失败。"))?;
            if owner.flatten().as_deref() != Some(project_id)
                || !complete_history_at_revision(
                    &transaction,
                    thread_id,
                    *source_updated_at,
                    *generation,
                    Some(fact_rule),
                )?
            {
                return Ok(None);
            }
        }
        fn read<T: serde::de::DeserializeOwned>(
            connection: &Connection,
            sql: &str,
            project_id: &str,
        ) -> Result<Vec<T>, AppError> {
            let mut query = connection
                .prepare(sql)
                .map_err(|_| AppError::store("读取项目来源材料失败。"))?;
            let rows = query
                .query_map([project_id], |row| row.get::<_, String>(0))
                .map_err(|_| AppError::store("查询项目来源材料失败。"))?;
            rows.map(|row| {
                serde_json::from_str(&row.map_err(|_| AppError::store("读取项目来源材料失败。"))?)
                    .map_err(|_| AppError::store("项目来源材料损坏。"))
            })
            .collect()
        }
        Ok(Some(ProjectMaterial {
            facts: read(&transaction, "SELECT f.fact_json FROM source_facts f JOIN thread_attributions a ON a.thread_id=f.thread_id WHERE a.project_id=?1 ORDER BY f.thread_id,f.ordinal,f.id", project_id)?,
            evidence: read(&transaction, "SELECT e.evidence_json FROM source_evidence e JOIN thread_attributions a ON a.thread_id=e.thread_id WHERE a.project_id=?1 ORDER BY e.thread_id,e.ordinal,e.id", project_id)?,
            items: read(&transaction, "SELECT i.item_json FROM history_items i JOIN history_turns t ON t.thread_id=i.thread_id AND t.id=i.turn_id JOIN thread_attributions a ON a.thread_id=i.thread_id WHERE a.project_id=?1 ORDER BY i.thread_id,t.ordinal,i.ordinal,i.id", project_id)?,
        }))
    }

    pub fn automatic_candidates(
        &self,
        project_id: &str,
    ) -> Result<Option<CandidatePreview>, AppError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT preview_json FROM automatic_candidate_views WHERE project_id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取旧候选清单失败。"))?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|_| AppError::store("旧候选清单缓存损坏。"))
        })
        .transpose()
    }

    pub fn cached_automatic_candidate_view_if_current(
        &self,
        project_id: &str,
        input_version: &str,
        expected: &[(String, i64, i64)],
        fact_rule: &str,
    ) -> Result<Option<(CandidatePreview, Vec<DerivedRelation>)>, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始读取候选缓存失败。"))?;
        for (thread_id, source_updated_at, generation) in expected {
            let owner: Option<Option<String>> = tx
                .query_row(
                    "SELECT project_id FROM thread_attributions WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选缓存项目归属失败。"))?;
            if owner.flatten().as_deref() != Some(project_id)
                || !complete_history_at_revision(
                    &tx,
                    thread_id,
                    *source_updated_at,
                    *generation,
                    Some(fact_rule),
                )?
            {
                return Ok(None);
            }
        }
        let saved: Option<(String, String)> = tx
            .query_row(
                "SELECT preview_json,relations_json FROM automatic_candidate_views WHERE project_id=?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| AppError::store("读取候选缓存失败。"))?;
        let Some((preview_json, relations_json)) = saved else {
            return Ok(None);
        };
        let preview: CandidatePreview =
            serde_json::from_str(&preview_json).map_err(|_| AppError::store("候选缓存损坏。"))?;
        if preview.input_version != input_version || !preview.stale_candidates.is_empty() {
            return Ok(None);
        }
        let relations = serde_json::from_str(&relations_json)
            .map_err(|_| AppError::store("规则关系缓存损坏。"))?;
        Ok(Some((preview, relations)))
    }

    pub fn replace_automatic_candidates(
        &self,
        preview: &CandidatePreview,
        relations: &[DerivedRelation],
    ) -> Result<(), AppError> {
        let preview_json =
            serde_json::to_string(preview).map_err(|_| AppError::store("序列化候选清单失败。"))?;
        let relations_json = serde_json::to_string(relations)
            .map_err(|_| AppError::store("序列化规则关系失败。"))?;
        self.connection()?.execute(
            "INSERT INTO automatic_candidate_views (project_id,preview_json,relations_json) VALUES (?1,?2,?3)
             ON CONFLICT(project_id) DO UPDATE SET preview_json=excluded.preview_json, relations_json=excluded.relations_json",
            params![preview.project_id, preview_json, relations_json],
        ).map_err(|_| AppError::store("保存规则关系和候选清单失败。"))?;
        Ok(())
    }

    pub fn replace_automatic_candidates_if_current(
        &self,
        preview: &CandidatePreview,
        relations: &[DerivedRelation],
        expected: &[(String, i64, i64)],
        fact_rule: &str,
    ) -> Result<bool, AppError> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存候选清单失败。"))?;
        for (thread_id, source_updated_at, generation) in expected {
            let owner: Option<Option<String>> = tx
                .query_row(
                    "SELECT project_id FROM thread_attributions WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选项目归属失败。"))?;
            if owner.flatten().as_deref() != Some(preview.project_id.as_str())
                || !complete_history_at_revision(
                    &tx,
                    thread_id,
                    *source_updated_at,
                    *generation,
                    Some(fact_rule),
                )?
            {
                return Ok(false);
            }
        }
        let preview_json =
            serde_json::to_string(preview).map_err(|_| AppError::store("序列化候选清单失败。"))?;
        let relations_json = serde_json::to_string(relations)
            .map_err(|_| AppError::store("序列化规则关系失败。"))?;
        tx.execute(
            "INSERT INTO automatic_candidate_views (project_id,preview_json,relations_json) VALUES (?1,?2,?3)
             ON CONFLICT(project_id) DO UPDATE SET preview_json=excluded.preview_json,relations_json=excluded.relations_json",
            params![preview.project_id, preview_json, relations_json],
        ).map_err(|_| AppError::store("保存规则关系和候选清单失败。"))?;
        tx.commit()
            .map_err(|_| AppError::store("提交规则关系和候选清单失败。"))?;
        Ok(true)
    }

    pub fn save_inferred_pair_outcome(&self, result: &InferredPairOutcome) -> Result<(), AppError> {
        let json =
            serde_json::to_string(result).map_err(|_| AppError::store("序列化推断关系失败。"))?;
        self.connection()?.execute(
            "INSERT INTO inferred_pair_outcomes(candidate_id,project_id,result_json) VALUES (?1,?2,?3)
             ON CONFLICT(candidate_id) DO UPDATE SET project_id=excluded.project_id,result_json=excluded.result_json",
            params![result.candidate_id, result.project_id, json],
        ).map_err(|_| AppError::store("保存推断关系失败。"))?;
        Ok(())
    }

    pub fn save_analysis_with_outcome(
        &self,
        run: &AnalysisRun,
        result: &InferredPairOutcome,
        expected_sources: &[(String, i64, i64)],
    ) -> Result<bool, AppError> {
        let run_json =
            serde_json::to_string(run).map_err(|_| AppError::store("序列化分析运行失败。"))?;
        let result_json =
            serde_json::to_string(result).map_err(|_| AppError::store("序列化推断关系失败。"))?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|_| AppError::store("开始保存分析结果失败。"))?;
        let latest_id: Option<String> = transaction
            .query_row(
                "SELECT id FROM analysis_runs WHERE project_id=?1 ORDER BY rowid DESC LIMIT 1",
                [&run.project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查最新分析运行失败。"))?;
        let persisted: Option<String> = transaction
            .query_row(
                "SELECT run_json FROM analysis_runs WHERE id=?1",
                [&run.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("检查分析运行状态失败。"))?;
        let active = persisted
            .and_then(|json| serde_json::from_str::<AnalysisRun>(&json).ok())
            .is_some_and(|saved| {
                saved.project_id == run.project_id
                    && saved.input_version == run.input_version
                    && saved.jev_base_url == run.jev_base_url
                    && saved.jev_model == run.jev_model
                    && saved.jev_config_revision == run.jev_config_revision
                    && saved.state == AnalysisRunState::Running
                    && saved.units.iter().any(|unit| {
                        unit.id == result.candidate_id
                            && unit.input_version == result.input_version
                            && unit.state == AnalysisUnitState::Running
                    })
            });
        if latest_id.as_deref() != Some(run.id.as_str()) || !active {
            return Ok(false);
        }
        for (thread_id, updated_at, generation) in expected_sources {
            let project: Option<String> = transaction
                .query_row(
                    "SELECT project_id FROM thread_attributions WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选项目归属失败。"))?
                .flatten();
            let actual: Option<i64> = transaction
                .query_row(
                    "SELECT updated_at FROM threads WHERE id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选来源版本失败。"))?;
            let current_generation: Option<i64> = transaction
                .query_row(
                    "SELECT generation FROM history_revisions WHERE thread_id=?1",
                    [thread_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| AppError::store("检查候选历史版本失败。"))?;
            if project.as_deref() != Some(run.project_id.as_str())
                || actual != Some(*updated_at)
                || current_generation.unwrap_or(0) != *generation
            {
                return Ok(false);
            }
        }
        transaction.execute(
            "INSERT INTO inferred_pair_outcomes(candidate_id,project_id,result_json) VALUES (?1,?2,?3)
             ON CONFLICT(candidate_id) DO UPDATE SET project_id=excluded.project_id,result_json=excluded.result_json",
            params![result.candidate_id, result.project_id, result_json],
        ).map_err(|_| AppError::store("保存推断关系失败。"))?;
        transaction
            .execute(
                "INSERT INTO analysis_runs(id,project_id,started_at,run_json) VALUES (?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET run_json=excluded.run_json",
                params![run.id, run.project_id, run.started_at_unix_ms, run_json],
            )
            .map_err(|_| AppError::store("保存分析运行失败。"))?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交分析结果失败，旧数据已保留。"))?;
        Ok(true)
    }

    pub fn inferred_pair_outcomes(
        &self,
        project_id: &str,
    ) -> Result<Vec<InferredPairOutcome>, AppError> {
        let connection = self.connection()?;
        let mut query = connection.prepare("SELECT result_json FROM inferred_pair_outcomes WHERE project_id=?1 ORDER BY candidate_id")
            .map_err(|_| AppError::store("读取推断关系失败。"))?;
        let rows = query
            .query_map([project_id], |row| row.get::<_, String>(0))
            .map_err(|_| AppError::store("查询推断关系失败。"))?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(|_| AppError::store("读取推断关系失败。"))?)
                .map_err(|_| AppError::store("推断关系缓存内容损坏。"))
        })
        .collect()
    }

    pub fn relation_reviews(
        &self,
    ) -> Result<Vec<(RelationReview, Option<InferredRelation>)>, AppError> {
        let connection = self.connection()?;
        let mut query = connection.prepare(
            "SELECT relation_id,project_id,decision,revision,confirmed_evidence_version,relation_json FROM relation_reviews ORDER BY relation_id"
        ).map_err(|_| AppError::store("读取关系裁决失败。"))?;
        let rows = query
            .query_map([], |row| {
                let decision: String = row.get(2)?;
                let decision = match decision.as_str() {
                    "confirmed" => UserRelationDecision::Confirmed,
                    "rejected" => UserRelationDecision::Rejected,
                    _ => UserRelationDecision::Pending,
                };
                Ok((
                    RelationReview {
                        relation_id: row.get(0)?,
                        project_id: row.get(1)?,
                        decision,
                        revision: row.get::<_, i64>(3)? as u64,
                        confirmed_evidence_version: row.get(4)?,
                    },
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|_| AppError::store("查询关系裁决失败。"))?;
        rows.map(|row| {
            let (review, json) = row.map_err(|_| AppError::store("读取关系裁决失败。"))?;
            let relation = json
                .map(|value| {
                    serde_json::from_str(&value).map_err(|_| AppError::store("关系裁决快照损坏。"))
                })
                .transpose()?;
            Ok((review, relation))
        })
        .collect()
    }

    pub fn save_relation_review(
        &self,
        project_id: &str,
        relation_id: &str,
        decision: UserRelationDecision,
        evidence_version: Option<&str>,
        expected_revision: u64,
        relation: Option<&InferredRelation>,
    ) -> Result<RelationReview, AppError> {
        let relation_json = relation
            .map(|item| {
                serde_json::to_string(item).map_err(|_| AppError::store("序列化关系裁决快照失败。"))
            })
            .transpose()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AppError::store("开始保存关系裁决失败。"))?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT revision FROM relation_reviews WHERE relation_id=?1",
                [relation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取关系修订号失败。"))?;
        if current.unwrap_or(0) != i64::try_from(expected_revision).unwrap_or(-1) {
            return Err(AppError::conflict());
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or_else(AppError::conflict)?;
        let revision_sql = i64::try_from(revision).map_err(|_| AppError::conflict())?;
        let status = match decision {
            UserRelationDecision::Pending => "pending",
            UserRelationDecision::Confirmed => "confirmed",
            UserRelationDecision::Rejected => "rejected",
        };
        let confirmed_evidence_version = if decision == UserRelationDecision::Confirmed {
            evidence_version.map(str::to_owned)
        } else {
            None
        };
        transaction.execute(
            "INSERT INTO relation_reviews(relation_id,project_id,decision,revision,confirmed_evidence_version,relation_json) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(relation_id) DO UPDATE SET project_id=excluded.project_id,decision=excluded.decision,revision=excluded.revision,confirmed_evidence_version=excluded.confirmed_evidence_version,relation_json=excluded.relation_json",
            params![relation_id, project_id, status, revision_sql, confirmed_evidence_version, relation_json],
        ).map_err(|_| AppError::store("保存关系裁决失败。"))?;
        transaction
            .commit()
            .map_err(|_| AppError::store("提交关系裁决失败。"))?;
        Ok(RelationReview {
            relation_id: relation_id.into(),
            project_id: project_id.into(),
            decision,
            revision,
            confirmed_evidence_version,
        })
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
            database_path: app_data_dir.join("sessions.sqlite3"),
            legacy_path: app_data_dir.join("preferences.json"),
        }
    }

    pub fn load(&self) -> Result<Preferences, AppError> {
        let connection = self.connection()?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT preferences_json FROM app_preferences WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| AppError::store("读取应用设置失败。"))?;
        if let Some(json) = stored {
            return serde_json::from_str(&json).map_err(|_| AppError::store("应用设置数据损坏。"));
        }
        let legacy = match fs::read(&self.legacy_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                AppError::store("旧应用设置文件无法读取，请检查 preferences.json。")
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Preferences::default())
            }
            Err(_) => return Err(AppError::store("读取旧应用设置失败，请检查目录权限。")),
        };
        self.save(&legacy)?;
        Ok(legacy)
    }

    pub fn save(&self, preferences: &Preferences) -> Result<(), AppError> {
        let json = serde_json::to_string(preferences)
            .map_err(|_| AppError::store("编码应用设置失败。"))?;
        self.connection()?
            .execute(
                "INSERT INTO app_preferences(id,preferences_json) VALUES (1,?1)
             ON CONFLICT(id) DO UPDATE SET preferences_json=excluded.preferences_json",
                [json],
            )
            .map_err(|_| {
                AppError::store("保存应用设置失败，请检查目录权限、空间或数据库写入冲突。")
            })?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, AppError> {
        let connection =
            Connection::open_with_flags(&self.database_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(|_| AppError::store("打开应用设置数据库失败，请检查应用数据目录。"))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|_| AppError::store("设置应用设置数据库等待时间失败。"))?;
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        DisplayTheme, ErrorCode, EvidenceField, FactKind, FactOutcome, HistoryItem,
        HistoryReadPath, HistoryTurn,
    };

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
        SessionStore::new(dir.clone()).unwrap();
        let store = PreferenceStore::new(dir.clone());
        store
            .save(&Preferences {
                selected_binary: Some("/tmp/codex".into()),
                theme: DisplayTheme::Dark,
                jev: Default::default(),
                jev_revision: 0,
            })
            .unwrap();
        let loaded = PreferenceStore::new(dir.clone()).load().unwrap();
        assert_eq!(loaded.selected_binary.as_deref(), Some("/tmp/codex"));
        assert!(matches!(loaded.theme, DisplayTheme::Dark));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_non_secret_settings_import_once_into_sqlite() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-settings-import-{nonce}"));
        fs::create_dir_all(&dir).unwrap();
        let old = Preferences {
            selected_binary: Some("/tmp/codex".into()),
            theme: DisplayTheme::Dark,
            jev: codexflow_domain::JevConfig {
                base_url: "https://api.typesafe.ai/gateway".into(),
                model: "jev-1.13.0".into(),
            },
            jev_revision: 3,
        };
        fs::write(
            dir.join("preferences.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        SessionStore::new(dir.clone()).unwrap();
        let settings = PreferenceStore::new(dir.clone());
        assert_eq!(settings.load().unwrap().jev.model, "jev-1.13.0");
        fs::remove_file(dir.join("preferences.json")).unwrap();
        let restored = PreferenceStore::new(dir.clone()).load().unwrap();
        assert_eq!(restored.jev.base_url, "https://api.typesafe.ai/gateway");
        assert_eq!(restored.jev_revision, 3);
        let stored: String = Connection::open(dir.join("sessions.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT preferences_json FROM app_preferences WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored.contains("API Key"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn workstream_name_and_failure_survive_restart() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-workstreams-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        let stream = Workstream {
            id: "stream".into(),
            project_id: "project".into(),
            name: "工作流 stream".into(),
            members: vec!["a".into(), "b".into()],
            relation_ids: vec!["ab".into()],
            algorithm_version: "test".into(),
            name_input_version: None,
            name_actual_model: None,
            name_error: None,
            predecessor_ids: Vec::new(),
        };
        store
            .replace_workstreams("project", 0, &[stream.clone()])
            .unwrap();
        store
            .save_workstream_name_error("project", "stream", &stream.members, "模型失败")
            .unwrap();
        assert_eq!(
            store.workstreams("project").unwrap()[0]
                .name_error
                .as_deref(),
            Some("模型失败")
        );
        assert!(!store
            .save_workstream_name("project", "stream", &["other".into()], "名称", "v1")
            .unwrap());
        assert!(store
            .save_workstream_name("project", "stream", &stream.members, "项目工作", "v1")
            .unwrap());
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let saved = reopened.workstreams("project").unwrap();
        assert_eq!(saved[0].name, "项目工作");
        assert_eq!(saved[0].name_input_version.as_deref(), Some("v1"));
        assert_eq!(saved[0].name_error, None);
        assert!(reopened.workstreams("other").unwrap().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn workstream_correction_revision_rejects_a_stale_concurrent_write() {
        use std::sync::{Arc, Barrier};
        let dir = std::env::temp_dir().join(format!(
            "codexflow-workstream-cas-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = Arc::new(SessionStore::new(dir.clone()).unwrap());
        store
            .replace_workstreams(
                "project",
                0,
                &[Workstream {
                    id: "stream".into(),
                    project_id: "project".into(),
                    name: "自动名称".into(),
                    members: vec!["a".into(), "b".into()],
                    relation_ids: vec![],
                    algorithm_version: "test".into(),
                    name_input_version: None,
                    name_actual_model: None,
                    name_error: None,
                    predecessor_ids: vec![],
                }],
            )
            .unwrap();
        let gate = Arc::new(Barrier::new(3));
        let handles: Vec<_> = ["名称甲", "名称乙"]
            .into_iter()
            .map(|name| {
                let store = Arc::clone(&store);
                let gate = Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    store.save_workstream_correction(
                        "project",
                        1,
                        WorkstreamChange::Rename { id: "stream", name },
                    )
                })
            })
            .collect();
        gate.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| result
                    .as_ref()
                    .err()
                    .is_some_and(|error| matches!(error.code, ErrorCode::ConcurrentModification)))
                .count(),
            1
        );
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let corrections = reopened.workstream_corrections("project").unwrap();
        assert_eq!(corrections.revision, 2);
        assert!(matches!(
            corrections.names.get("stream").map(String::as_str),
            Some("名称甲" | "名称乙")
        ));
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
            .execute_batch("PRAGMA user_version = 16;")
            .unwrap();
        drop(connection);
        let error = SessionStore::new(dir.clone())
            .err()
            .expect("migration error");
        assert!(matches!(error.code, ErrorCode::DatabaseTooNew));
        let desktop_error = serde_json::to_value(&error).unwrap();
        assert_eq!(desktop_error["code"], "DATABASE_TOO_NEW");
        assert_eq!(desktop_error["retryable"], false);
        assert_eq!(desktop_error["cachePreserved"], true);
        assert!(desktop_error["nextStep"]
            .as_str()
            .unwrap()
            .contains("较新版本"));
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 16);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_later_migration_rolls_back_earlier_schema_steps() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-migration-rollback-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        store
            .save_collection(&[thread("原缓存", 200, false, 1_000)], &[])
            .unwrap();
        drop(store);
        let path = dir.join("sessions.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TABLE relation_reviews;
             DROP TABLE workstreams;
             DROP TABLE workstream_member_corrections;
             CREATE TABLE workstream_member_corrections (wrong_column TEXT);
             PRAGMA user_version = 11;",
            )
            .unwrap();
        drop(connection);

        let error = SessionStore::new(dir.clone())
            .err()
            .expect("migration must fail");
        assert!(matches!(error.code, ErrorCode::MigrationFailed));
        assert!(error.cache_preserved);
        assert!(serde_json::to_value(&error).unwrap()["nextStep"].is_string());
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        let relation_reviews: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='relation_reviews')",
            [], |row| row.get(0),
        ).unwrap();
        assert!(!relation_reviews);
        let title: String = connection.query_row(
            "SELECT json_extract(metadata_json, '$.title') FROM threads WHERE id='duplicate-thread'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(title, "原缓存");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn relation_review_revision_survives_restart_and_serializes_concurrent_writes() {
        use std::sync::{Arc, Barrier};
        let nonce = now_ms();
        let dir = std::env::temp_dir().join(format!("codexflow-review-{nonce}"));
        let store = Arc::new(SessionStore::new(dir.clone()).unwrap());
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = [
            UserRelationDecision::Confirmed,
            UserRelationDecision::Rejected,
        ]
        .into_iter()
        .map(|decision| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store.save_relation_review(
                    "project",
                    "inferred:one",
                    decision,
                    Some("evidence-v1"),
                    0,
                    None,
                )
            })
        })
        .collect();
        barrier.wait();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    Err(AppError {
                        code: ErrorCode::ConcurrentModification,
                        ..
                    })
                ))
                .count(),
            1
        );
        drop(store);
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let review = reopened.relation_reviews().unwrap().remove(0).0;
        assert_eq!(review.revision, 1);
        assert_eq!(
            review.confirmed_evidence_version.is_some(),
            review.decision == UserRelationDecision::Confirmed
        );
        let restored = reopened
            .save_relation_review(
                "project",
                "inferred:one",
                UserRelationDecision::Pending,
                None,
                review.revision,
                None,
            )
            .unwrap();
        assert_eq!(restored.revision, 2);
        assert_eq!(restored.confirmed_evidence_version, None);
        assert_eq!(
            reopened.relation_reviews().unwrap()[0].0.decision,
            UserRelationDecision::Pending
        );
        let moved = reopened
            .save_relation_review(
                "another-project",
                "inferred:one",
                UserRelationDecision::Rejected,
                None,
                restored.revision,
                None,
            )
            .unwrap();
        assert_eq!(moved.revision, 3);
        assert_eq!(
            reopened.relation_reviews().unwrap()[0].0.project_id,
            "another-project"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn either_issue_v8_database_upgrades_to_the_complete_v13_schema() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        for name in ["summary-v8", "candidates-v8"] {
            let dir = std::env::temp_dir().join(format!("codexflow-{name}-{nonce}"));
            let initialized = SessionStore::new(dir.clone()).unwrap();
            drop(initialized);
            let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
            if name == "summary-v8" {
                connection
                    .execute(
                        "INSERT INTO thread_summaries VALUES ('preserved-summary', '{}')",
                        [],
                    )
                    .unwrap();
                connection
                    .execute_batch("DROP TABLE automatic_candidate_views;")
                    .unwrap();
            } else {
                connection
                    .execute(
                        "INSERT INTO automatic_candidate_views VALUES ('preserved-project', '{}', '[]')",
                        [],
                    )
                    .unwrap();
                connection
                    .execute_batch("DROP TABLE summary_runs; DROP TABLE thread_summaries;")
                    .unwrap();
            }
            connection
                .execute_batch("DROP TABLE app_preferences; PRAGMA user_version = 8;")
                .unwrap();
            drop(connection);

            let store = SessionStore::new(dir.clone()).unwrap();
            let connection = store.connection().unwrap();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 15);
            for table in [
                "automatic_candidate_views",
                "thread_summaries",
                "summary_runs",
                "analysis_runs",
                "inferred_pair_outcomes",
                "relation_reviews",
            ] {
                let exists: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                        [table],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(exists, "missing table {table} after upgrading {name}");
            }
            if name == "summary-v8" {
                let preserved: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM thread_summaries WHERE thread_id='preserved-summary'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(preserved, 1);
            } else {
                let preserved: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM automatic_candidate_views WHERE project_id='preserved-project'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(preserved, 1);
            }
            drop(connection);
            drop(store);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn unfinished_summary_run_is_recovered_after_restart() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-summary-recovery-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        let run = SummaryRun {
            id: "summary-run".into(),
            thread_id: "thread-h".into(),
            state: SummaryRunState::Cancelling,
            model: "model".into(),
            started_at_unix_ms: 1,
            finished_at_unix_ms: None,
            temporary_thread_id: Some("temporary".into()),
            turn_id: Some("turn".into()),
            reused_cache: false,
            error: None,
        };
        store.save_summary_run(&run).unwrap();
        drop(store);
        let reopened = SessionStore::new(dir.clone()).unwrap();
        let recovered = reopened.summary_run("summary-run").unwrap().unwrap();
        assert_eq!(recovered.state, SummaryRunState::Failed);
        assert!(recovered.finished_at_unix_ms.is_some());
        assert!(recovered.error.is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn automatic_candidates_replace_one_project_snapshot_without_duplicates() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-candidates-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        let preview = CandidatePreview {
            project_id: "project".into(),
            input_version: String::new(),
            thread_count: 2,
            unavailable_threads: 0,
            neighbor_limit: 10,
            candidate_count: 0,
            candidates: Vec::new(),
            candidate_versions: std::collections::BTreeMap::new(),
            stale_candidates: Vec::new(),
        };
        let relation = DerivedRelation {
            id: "derived-one".into(),
            project_id: "project".into(),
            from_thread_id: "a".into(),
            to_thread_id: "b".into(),
            kind: codexflow_domain::DerivedRelationKind::SharedFile,
            source: "derived".into(),
            basis: "同一文件".into(),
            evidence: Vec::new(),
        };
        store
            .replace_automatic_candidates(&preview, &[relation.clone()])
            .unwrap();
        store
            .replace_automatic_candidates(&preview, &[relation])
            .unwrap();
        let connection = store.connection().unwrap();
        let (count, json): (i64, String) = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM automatic_candidate_views), relations_json FROM automatic_candidate_views WHERE project_id='project'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(count, 1);
        assert_eq!(
            serde_json::from_str::<Vec<DerivedRelation>>(&json)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .cached_automatic_candidate_view_if_current("project", "v1", &[], "rule")
            .unwrap()
            .is_none());
        let mut current = preview.clone();
        current.input_version = "v1".into();
        store.replace_automatic_candidates(&current, &[]).unwrap();
        assert!(store
            .cached_automatic_candidate_view_if_current("project", "v1", &[], "rule")
            .unwrap()
            .is_some());
        assert!(store
            .cached_automatic_candidate_view_if_current("project", "v2", &[], "rule")
            .unwrap()
            .is_none());
        assert!(store
            .cached_automatic_candidate_view_if_current(
                "project",
                "v1",
                &[("missing".into(), 1, 1)],
                "rule",
            )
            .unwrap()
            .is_none());
        drop(connection);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn history_pages_and_locations_survive_partial_retry_and_reopen() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-history-store-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        store
            .save_collection(&[thread("历史会话", 200, false, 1_000)], &[])
            .unwrap();
        let make_turn = |id: &str, ordinal, source_updated_at| HistoryTurn {
            thread_id: "duplicate-thread".into(),
            id: id.into(),
            ordinal,
            status: "completed".into(),
            started_at_unix_ms: Some(100_000),
            completed_at_unix_ms: Some(102_000),
            duration_ms: Some(2_000),
            time_error: None,
            source_updated_at,
            content_version: format!("turn-{id}"),
        };
        let make_item = |id: &str, turn_id: &str, ordinal, source_updated_at| HistoryItem {
            thread_id: "duplicate-thread".into(),
            turn_id: turn_id.into(),
            id: id.into(),
            ordinal,
            source_type: "agentMessage".into(),
            supported: true,
            text: Some(format!("正文 {turn_id}/{id}")),
            command: None,
            cwd: None,
            output: None,
            exit_code: None,
            status: None,
            changes: vec![],
            source_updated_at,
            content_version: format!("item-{turn_id}-{id}"),
        };
        let coverage = HistoryCoverage {
            thread_id: "duplicate-thread".into(),
            source_updated_at: 200,
            attempted_at_unix_ms: 2_000,
            path: HistoryReadPath::Paginated,
            turns_complete: true,
            items_complete: true,
            turn_pages: 1,
            item_pages: 1,
            loaded_turns: 2,
            loaded_items: 2,
            incompatible: false,
            error: None,
        };
        store
            .save_history(&HistorySnapshot {
                coverage: coverage.clone(),
                turns: vec![make_turn("turn-1", 0, 200), make_turn("turn-2", 1, 200)],
                items: vec![
                    make_item("item-1", "turn-1", 0, 200),
                    make_item("item-1", "turn-2", 1, 200),
                ],
            })
            .unwrap();
        store
            .save_projects_and_attributions(
                &[LocalProject {
                    id: "project".into(),
                    name: "项目".into(),
                    root: "/tmp/example".into(),
                    git_common_dir: None,
                }],
                &[ThreadAttribution {
                    thread_id: "duplicate-thread".into(),
                    project_id: Some("project".into()),
                    workspace_root: None,
                    basis: "test".into(),
                    detail: "test".into(),
                    diagnostic: None,
                    source_project_id: None,
                }],
                None,
            )
            .unwrap();
        assert_eq!(store.project_turns("project").unwrap().len(), 2);
        assert!(store.project_turns("other-project").unwrap().is_empty());
        assert_eq!(
            store.history_turns("duplicate-thread", 1, 1).unwrap().turns[0].id,
            "turn-2"
        );
        let location = store
            .locate_history_item("duplicate-thread", "turn-2", "item-1")
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                location.turn_id.as_str(),
                location.turn_offset,
                location.offset
            ),
            ("turn-2", 1, 0)
        );
        assert_eq!(
            store
                .locate_history_item("duplicate-thread", "turn-1", "item-1")
                .unwrap()
                .unwrap()
                .turn_offset,
            0
        );
        assert!(store
            .locate_history_item("duplicate-thread", "turn-2", "missing")
            .unwrap()
            .is_none());
        assert!(
            store
                .thread("duplicate-thread")
                .unwrap()
                .unwrap()
                .content_complete
        );

        store
            .save_collection(&[thread("历史更新", 201, false, 3_000)], &[])
            .unwrap();
        let mut partial = coverage;
        partial.source_updated_at = 201;
        partial.attempted_at_unix_ms = 4_000;
        partial.turns_complete = false;
        partial.items_complete = false;
        partial.loaded_turns = 1;
        partial.loaded_items = 1;
        partial.error = Some("第二页失败".into());
        store
            .save_history(&HistorySnapshot {
                coverage: partial,
                turns: vec![make_turn("turn-1", 0, 201)],
                items: vec![make_item("item-1", "turn-1", 0, 201)],
            })
            .unwrap();
        let reopened = SessionStore::new(dir.clone()).unwrap();
        assert!(
            !reopened
                .thread("duplicate-thread")
                .unwrap()
                .unwrap()
                .content_complete
        );
        assert_eq!(
            reopened
                .history_coverage("duplicate-thread")
                .unwrap()
                .unwrap()
                .loaded_items,
            1
        );
        assert_eq!(
            reopened
                .history_items("duplicate-thread", "turn-2", 0, 20)
                .unwrap()
                .items[0]
                .id,
            "item-1"
        );
        assert_eq!(
            reopened
                .history_items("duplicate-thread", "turn-2", 0, 20)
                .unwrap()
                .items[0]
                .source_updated_at,
            200
        );
        assert_eq!(
            reopened
                .history_items("duplicate-thread", "turn-1", 0, 20)
                .unwrap()
                .items[0]
                .text
                .as_deref(),
            Some("正文 turn-1/item-1")
        );
        assert_eq!(
            reopened
                .history_items("duplicate-thread", "turn-2", 0, 20)
                .unwrap()
                .items[0]
                .text
                .as_deref(),
            Some("正文 turn-2/item-1")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_four_cache_gains_history_tables_without_losing_threads() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-history-migration-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        store
            .save_collection(&[thread("旧缓存", 200, false, 1_000)], &[])
            .unwrap();
        drop(store);
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        connection
            .execute_batch(
                "DROP TABLE app_preferences; DROP TABLE history_coverage; DROP TABLE history_turns; DROP TABLE history_items;
             PRAGMA user_version = 4;",
            )
            .unwrap();
        drop(connection);
        let upgraded = SessionStore::new(dir.clone()).unwrap();
        assert_eq!(
            upgraded
                .thread("duplicate-thread")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("旧缓存")
        );
        assert_eq!(
            upgraded
                .history_turns("duplicate-thread", 0, 20)
                .unwrap()
                .total,
            0
        );
        let version: i64 = Connection::open(dir.join("sessions.sqlite3"))
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 15);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_five_item_cache_migrates_without_losing_content() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("codexflow-item-identity-migration-{nonce}"));
        let store = SessionStore::new(dir.clone()).unwrap();
        drop(store);
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        connection
            .execute_batch(
                "DROP TABLE app_preferences; DROP TABLE history_items;
             CREATE TABLE history_items (
                thread_id TEXT NOT NULL, id TEXT NOT NULL, turn_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL, item_json TEXT NOT NULL,
                PRIMARY KEY (thread_id, id)
             );
             CREATE INDEX history_item_order ON history_items(thread_id, turn_id, ordinal, id);
             PRAGMA user_version = 5;",
            )
            .unwrap();
        let item = HistoryItem {
            thread_id: "thread-legacy".into(),
            turn_id: "turn-legacy".into(),
            id: "item-legacy".into(),
            ordinal: 3,
            source_type: "agentMessage".into(),
            supported: true,
            text: Some("旧内容".into()),
            command: None,
            cwd: None,
            output: None,
            exit_code: None,
            status: None,
            changes: vec![],
            source_updated_at: 200,
            content_version: "old-hash".into(),
        };
        let json = serde_json::to_string(&item).unwrap();
        connection.execute(
            "INSERT INTO history_items (thread_id,id,turn_id,ordinal,item_json) VALUES (?1,?2,?3,?4,?5)",
            params![item.thread_id, item.id, item.turn_id, item.ordinal, json],
        ).unwrap();
        drop(connection);

        let upgraded = SessionStore::new(dir.clone()).unwrap();
        let page = upgraded
            .history_items("thread-legacy", "turn-legacy", 0, 20)
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].text.as_deref(), Some("旧内容"));
        assert_eq!(page.items[0].content_version, "old-hash");
        let connection = Connection::open(dir.join("sessions.sqlite3")).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 15);
        let mut second = item.clone();
        second.turn_id = "turn-new".into();
        connection.execute(
            "INSERT INTO history_items (thread_id,turn_id,id,ordinal,item_json) VALUES (?1,?2,?3,?4,?5)",
            params![second.thread_id, second.turn_id, second.id, second.ordinal,
                serde_json::to_string(&second).unwrap()],
        ).unwrap();
        assert_eq!(
            upgraded
                .history_items("thread-legacy", "turn-new", 0, 20)
                .unwrap()
                .total,
            1
        );
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
        assert_eq!((version, count), (15, 1));
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
        assert_eq!(version, 15);
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
            assert_eq!(version, 15);
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

    #[test]
    fn evidence_snapshot_keeps_old_item_fact_and_pointer_together_during_refresh() {
        use std::sync::mpsc;
        use std::thread as os_thread;
        use std::time::Duration;

        let dir = std::env::temp_dir().join(format!(
            "codexflow-evidence-snapshot-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = SessionStore::new(dir.clone()).unwrap();
        let path = dir.join("sessions.sqlite3");
        let connection = Connection::open(&path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(connection);
        store
            .save_collection(&[thread("证据并发", 100, false, 1)], &[])
            .unwrap();
        let old_item = HistoryItem {
            thread_id: "duplicate-thread".into(),
            turn_id: "turn".into(),
            id: "item".into(),
            ordinal: 0,
            source_type: "commandExecution".into(),
            supported: true,
            text: None,
            command: Some("cargo test".into()),
            cwd: None,
            output: None,
            exit_code: Some(0),
            status: Some("completed".into()),
            changes: vec![],
            source_updated_at: 100,
            content_version: "v1".into(),
        };
        store
            .save_history(&HistorySnapshot {
                coverage: HistoryCoverage {
                    thread_id: "duplicate-thread".into(),
                    source_updated_at: 100,
                    attempted_at_unix_ms: 1,
                    path: HistoryReadPath::FullRead,
                    turns_complete: true,
                    items_complete: true,
                    turn_pages: 0,
                    item_pages: 0,
                    loaded_turns: 1,
                    loaded_items: 1,
                    incompatible: false,
                    error: None,
                },
                turns: vec![HistoryTurn {
                    thread_id: "duplicate-thread".into(),
                    id: "turn".into(),
                    ordinal: 0,
                    status: "completed".into(),
                    started_at_unix_ms: None,
                    completed_at_unix_ms: None,
                    duration_ms: None,
                    time_error: None,
                    source_updated_at: 100,
                    content_version: "turn-v1".into(),
                }],
                items: vec![old_item.clone()],
            })
            .unwrap();
        let old_fact = SourceFact {
            id: "fact".into(),
            thread_id: "duplicate-thread".into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            kind: FactKind::Command,
            subject: "cargo test".into(),
            operation: "executed".into(),
            outcome: FactOutcome::Succeeded,
            evidence_id: "evidence".into(),
            content_version: "v1".into(),
            rule_version: "test".into(),
        };
        let old_evidence = SourceEvidence {
            id: "evidence".into(),
            fact_id: "fact".into(),
            thread_id: "duplicate-thread".into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            field: EvidenceField::Command,
            change_index: None,
            excerpt: "cargo test".into(),
            content_version: "v1".into(),
        };
        store
            .replace_automatic_facts(
                "duplicate-thread",
                100,
                "v1",
                "test",
                store.history_generation("duplicate-thread").unwrap(),
                &[old_fact.clone()],
                &[old_evidence.clone()],
            )
            .unwrap();

        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let writer = os_thread::spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let mut connection = Connection::open(path).unwrap();
            connection.busy_timeout(Duration::from_secs(3)).unwrap();
            let tx = connection.transaction().unwrap();
            let mut new_item = old_item;
            new_item.command = Some("cargo build".into());
            new_item.content_version = "v2".into();
            let mut new_fact = old_fact;
            new_fact.subject = "cargo build".into();
            new_fact.content_version = "v2".into();
            let mut new_evidence = old_evidence;
            new_evidence.excerpt = "cargo build".into();
            new_evidence.content_version = "v2".into();
            tx.execute("UPDATE history_items SET item_json=?1 WHERE thread_id='duplicate-thread' AND turn_id='turn' AND id='item'",
                [serde_json::to_string(&new_item).unwrap()]).unwrap();
            tx.execute(
                "UPDATE source_facts SET fact_json=?1 WHERE id='fact'",
                [serde_json::to_string(&new_fact).unwrap()],
            )
            .unwrap();
            tx.execute(
                "UPDATE source_evidence SET evidence_json=?1 WHERE id='evidence'",
                [serde_json::to_string(&new_evidence).unwrap()],
            )
            .unwrap();
            tx.commit().unwrap();
            done_tx.send(()).unwrap();
        });
        let during = store
            .read_evidence_snapshot(None, Some("evidence"), || {
                start_tx.send(()).unwrap();
                done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            })
            .unwrap()
            .unwrap();
        writer.join().unwrap();
        assert_eq!(during.evidence.content_version, "v1");
        assert_eq!(during.item.unwrap().content_version, "v1");
        assert_eq!(during.fact.unwrap().content_version, "v1");
        assert_eq!(during.location.unwrap().offset, 0);
        let after = store.stored_evidence_snapshot("evidence").unwrap().unwrap();
        assert_eq!(after.evidence.content_version, "v2");
        assert_eq!(after.item.unwrap().content_version, "v2");
        assert_eq!(after.fact.unwrap().content_version, "v2");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn identical_history_keeps_revision_and_failed_reread_keeps_complete_cache() {
        let dir = std::env::temp_dir().join(format!(
            "codexflow-history-revision-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = SessionStore::new(dir.clone()).unwrap();
        store
            .save_collection(&[thread("source", 20, false, 1)], &[])
            .unwrap();
        let mut snapshot = HistorySnapshot {
            coverage: HistoryCoverage {
                thread_id: "duplicate-thread".into(),
                source_updated_at: 20,
                attempted_at_unix_ms: 1,
                path: HistoryReadPath::FullRead,
                turns_complete: true,
                items_complete: true,
                turn_pages: 0,
                item_pages: 0,
                loaded_turns: 0,
                loaded_items: 0,
                incompatible: false,
                error: None,
            },
            turns: vec![],
            items: vec![],
        };
        store.save_history(&snapshot).unwrap();
        assert_eq!(store.history_generation("duplicate-thread").unwrap(), 1);
        snapshot.coverage.attempted_at_unix_ms = 2;
        store.save_history(&snapshot).unwrap();
        assert_eq!(store.history_generation("duplicate-thread").unwrap(), 1);
        snapshot.coverage.turns_complete = false;
        snapshot.coverage.items_complete = false;
        snapshot.coverage.error = Some("临时读取失败".into());
        let retained = store.save_history(&snapshot).unwrap();
        assert!(retained.items_complete);
        assert_eq!(retained.error.as_deref(), Some("临时读取失败"));
        assert_eq!(store.history_generation("duplicate-thread").unwrap(), 1);
        drop(store);
        let reopened = SessionStore::new(dir.clone()).unwrap();
        assert_eq!(reopened.history_generation("duplicate-thread").unwrap(), 1);
        assert_eq!(
            reopened
                .history_coverage("duplicate-thread")
                .unwrap()
                .unwrap()
                .error,
            Some("临时读取失败".into())
        );
        reopened
            .save_collection(&[thread("new source", 21, false, 3)], &[])
            .unwrap();
        assert!(reopened.save_history(&snapshot).is_err());
        assert_eq!(reopened.history_generation("duplicate-thread").unwrap(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
