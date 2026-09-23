use codexflow_domain::{AppError, ListScopeStatus, Preferences, SessionList, ThreadMetadata};
use rusqlite::{params, Connection};
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
        if version > 1 {
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
            let json = serde_json::to_string(thread)
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
        }
        transaction
            .commit()
            .map_err(|_| AppError::store("提交会话列表失败，旧缓存已保留。"))
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
        Ok(SessionList { threads, scopes })
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
            .execute_batch("PRAGMA user_version = 2;")
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
        assert_eq!(version, 2);
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
}
