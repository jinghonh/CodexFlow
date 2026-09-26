use super::SourceService;
use codexflow_domain::{AppError, ErrorCode, TextConfig, TextStatus, TextValidation};
use codexflow_jev::Credential;
use codexflow_text::{normalize_text_base_url, TextClient, TextCredential, TextOutput};
use tokio::sync::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

fn credential_error() -> AppError {
    AppError::text(
        ErrorCode::TextCredentialFailed,
        "无法访问 macOS 钥匙串。请解锁并允许应用访问后重试。",
        true,
    )
}
fn not_configured() -> AppError {
    AppError::text(
        ErrorCode::TextNotConfigured,
        "请先保存文本服务地址、模型及当前地址的 API Key。",
        false,
    )
}

impl SourceService {
    pub async fn text_status(&self) -> TextStatus {
        let config = self.preferences.lock().await.text.clone();
        let (credential_configured, credential_error) = match self.text_credentials.load() {
            Ok(value) => (
                value.is_some_and(|item| item.base_url == config.base_url),
                None,
            ),
            Err(_) => (false, Some(credential_error())),
        };
        TextStatus {
            config,
            credential_configured,
            credential_error,
        }
    }

    pub async fn save_text(
        &self,
        base_url: String,
        model: String,
        api_key: Option<String>,
    ) -> Result<TextStatus, AppError> {
        let raw_base_url = base_url;
        let base_url = normalize_text_base_url(&raw_base_url)?;
        let model = model.trim().to_owned();
        if model.is_empty() || model.chars().count() > 200 || model.chars().any(char::is_control) {
            return Err(AppError::text(
                ErrorCode::AnalysisModelUnsupported,
                "请填写有效的文本模型 ID。",
                false,
            ));
        }
        if api_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.chars().any(char::is_control))
        {
            return Err(not_configured());
        }
        let _gate = self.stop_text().await;
        let mut preferences = self.preferences.lock().await;
        if preferences.text.base_url != base_url && api_key.is_none() {
            return Err(AppError::text(
                ErrorCode::TextNotConfigured,
                "更换文本服务地址时请填写新 API Key；旧密钥不会发送到新地址。",
                false,
            ));
        }
        let old_credential = self
            .text_credentials
            .load()
            .map_err(|_| credential_error())?;
        let contains_key = api_key
            .as_deref()
            .map(str::trim)
            .into_iter()
            .chain(old_credential.as_ref().map(|item| item.key.as_str()))
            .any(|key| raw_base_url.contains(key) || base_url.contains(key) || model.contains(key));
        if contains_key {
            return Err(AppError::text(
                ErrorCode::TextInvalidAddress,
                "服务地址和模型 ID 不能包含 API Key。",
                false,
            ));
        }
        let replacing = api_key.is_some();
        if let Some(key) = api_key {
            self.text_credentials
                .save(&Credential {
                    base_url: base_url.clone(),
                    key: key.trim().into(),
                })
                .map_err(|_| credential_error())?;
        }
        let mut next = preferences.clone();
        let config = TextConfig { base_url, model };
        if next.text != config {
            next.text_revision = next.text_revision.saturating_add(1);
        }
        next.text = config;
        if let Err(error) = self.store.save(&next) {
            if replacing {
                let restored = if let Some(previous) = old_credential {
                    self.text_credentials.save(&previous)
                } else {
                    self.text_credentials.delete()
                };
                if restored.is_err() {
                    return Err(credential_error());
                }
            }
            return Err(error);
        }
        *preferences = next;
        drop(preferences);
        Ok(self.text_status().await)
    }

    pub async fn delete_text_credential(&self) -> Result<TextStatus, AppError> {
        let _gate = self.stop_text().await;
        self.text_credentials
            .delete()
            .map_err(|_| credential_error())?;
        Ok(self.text_status().await)
    }

    async fn stop_text(&self) -> RwLockWriteGuard<'_, ()> {
        self.text_cancel.lock().await.cancel();
        let gate = self.text_gate.write().await;
        *self.text_cancel.lock().await = CancellationToken::new();
        gate
    }

    pub async fn cancel_text_request(&self) {
        let mut token = self.text_validation_cancel.lock().await;
        token.cancel();
        *token = CancellationToken::new();
    }

    pub(crate) async fn text_config(&self) -> TextConfig {
        self.preferences.lock().await.text.clone()
    }

    pub(crate) async fn text_complete(
        &self,
        expected: &TextConfig,
        expected_revision: Option<u64>,
        prompt: &str,
        cancel: CancellationToken,
    ) -> Result<TextOutput, AppError> {
        let _gate = self.text_gate.read().await;
        let settings_cancel = self.text_cancel.lock().await.clone();
        let preferences = self.preferences.lock().await.clone();
        let config = preferences.text;
        if config != *expected
            || expected_revision.is_some_and(|revision| revision != preferences.text_revision)
        {
            return Err(AppError::text(
                ErrorCode::AnalysisConfigChanged,
                "文本服务设置已变化，旧请求未发起。",
                false,
            ));
        }
        if config.base_url.is_empty() || config.model.is_empty() {
            return Err(not_configured());
        }
        let credential = self
            .text_credentials
            .load()
            .map_err(|_| credential_error())?
            .filter(|item| item.base_url == config.base_url)
            .ok_or_else(not_configured)?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AppError::text(ErrorCode::AnalysisCancelled,
                "文本服务请求已取消；远端计算或计费可能继续。", false)),
            _ = settings_cancel.cancelled() => Err(AppError::text(ErrorCode::AnalysisConfigChanged,
                "文本服务设置已变化，旧请求结果不会保存。", false)),
            result = async {
                TextClient::new()?.complete(&TextCredential { base_url: credential.base_url,
                    key: credential.key }, &config.model, prompt, cancel.clone()).await
            } => result,
        }
    }

    pub async fn validate_text(&self) -> Result<TextValidation, AppError> {
        // The fixture is fixed and contains no project history.
        const FIXTURE: &str =
            "这是连接验证。只回复 JSON：{\"ok\":true}。材料：合成会话甲已完成测试。";
        let config = self.text_config().await;
        let cancel = self.text_validation_cancel.lock().await.clone();
        let _slot = self
            .model_slots
            .acquire_many(crate::analysis_batch::model_slot_weight(2))
            .await
            .map_err(|_| not_configured())?;
        let output = self.text_complete(&config, None, FIXTURE, cancel).await?;
        if serde_json::from_str::<serde_json::Value>(&output.text).ok()
            != Some(serde_json::json!({"ok": true}))
        {
            return Err(AppError::text(
                ErrorCode::TextProtocolInvalid,
                "文本服务未按固定合成任务返回 JSON；请检查模型与兼容协议。",
                false,
            ));
        }
        Ok(TextValidation {
            requested_model: output.requested_model,
            actual_model: output.actual_model,
            reply: output.text.chars().take(300).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmptyCredentials;
    use codexflow_domain::{
        HistoryCoverage, HistoryItem, HistoryReadPath, HistorySnapshot, HistoryTurn,
        SummaryRunState, ThreadMetadata,
    };
    use codexflow_jev::CredentialStore;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[derive(Default)]
    struct MemoryCredentials(Mutex<Option<Credential>>);
    impl CredentialStore for MemoryCredentials {
        fn load(&self) -> Result<Option<Credential>, AppError> {
            Ok(self.0.lock().unwrap().as_ref().map(|item| Credential {
                base_url: item.base_url.clone(),
                key: item.key.clone(),
            }))
        }
        fn save(&self, item: &Credential) -> Result<(), AppError> {
            *self.0.lock().unwrap() = Some(Credential {
                base_url: item.base_url.clone(),
                key: item.key.clone(),
            });
            Ok(())
        }
        fn delete(&self) -> Result<(), AppError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    struct LockedCredentials;
    impl CredentialStore for LockedCredentials {
        fn load(&self) -> Result<Option<Credential>, AppError> {
            Err(AppError::jev(
                ErrorCode::JevCredentialFailed,
                "钥匙串不可用",
                true,
            ))
        }
        fn save(&self, _: &Credential) -> Result<(), AppError> {
            self.load().map(|_| ())
        }
        fn delete(&self) -> Result<(), AppError> {
            self.load().map(|_| ())
        }
    }

    #[tokio::test]
    async fn settings_keep_secrets_out_of_database_and_bind_key_to_address() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codexflow-text-settings-{nonce}"));
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credential_stores(
            root.clone(),
            Arc::new(EmptyCredentials),
            credentials,
        )
        .unwrap();
        let saved = service
            .save_text(
                "http://127.0.0.1:41001/gateway/v1/".into(),
                "requested-model".into(),
                Some("test-secret-key-one".into()),
            )
            .await
            .unwrap();
        assert!(service
            .save_text(
                "https://example.test/test-secret-key-one/v1".into(),
                "requested-model".into(),
                Some("test-secret-key-one".into())
            )
            .await
            .is_err());
        assert_eq!(saved.config.base_url, "http://127.0.0.1:41001/gateway/v1");
        assert!(saved.credential_configured);
        assert_eq!(service.store.load().unwrap().text_revision, 1);
        service
            .save_text(
                saved.config.base_url.clone(),
                saved.config.model.clone(),
                Some("test-secret-key-two".into()),
            )
            .await
            .unwrap();
        assert_eq!(service.store.load().unwrap().text_revision, 1);
        assert!(service
            .save_text(
                "http://127.0.0.1:41002/v1".into(),
                "requested-model".into(),
                None
            )
            .await
            .is_err());
        assert!(service.text_status().await.credential_configured);
        let changed = service
            .save_text(
                "http://127.0.0.1:41002/v1".into(),
                "requested-model".into(),
                Some("test-secret-key-three".into()),
            )
            .await
            .unwrap();
        assert!(changed.credential_configured);
        assert_eq!(service.store.load().unwrap().text_revision, 2);
        let bytes = fs::read(root.join("sessions.sqlite3")).unwrap();
        for secret in [
            b"test-secret-key-one".as_slice(),
            b"test-secret-key-two",
            b"test-secret-key-three",
        ] {
            assert!(!bytes.windows(secret.len()).any(|part| part == secret));
        }
        let deleted = service.delete_text_credential().await.unwrap();
        assert!(!deleted.credential_configured);
        assert_eq!(deleted.config.base_url, "http://127.0.0.1:41002/v1");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn locked_keychain_keeps_nonsecret_settings_and_never_saves_plaintext() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codexflow-text-locked-{nonce}"));
        let service = SourceService::with_credential_stores(
            root.clone(),
            Arc::new(EmptyCredentials),
            Arc::new(LockedCredentials),
        )
        .unwrap();
        let status = service.text_status().await;
        assert!(matches!(
            status.credential_error.unwrap().code,
            ErrorCode::TextCredentialFailed
        ));
        let error = service
            .save_text(
                "https://example.test/v1".into(),
                "model".into(),
                Some("test-secret-key".into()),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(error.code, ErrorCode::TextCredentialFailed));
        assert_eq!(service.store.load().unwrap().text, TextConfig::default());
        assert!(!fs::read(root.join("sessions.sqlite3"))
            .unwrap()
            .windows(b"test-secret-key".len())
            .any(|part| part == b"test-secret-key"));
        let _ = fs::remove_dir_all(root);
    }

    fn server(responses: Vec<&'static str>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            responses.into_iter().map(|body| {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") else { continue };
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers.lines().find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
                if request.len() >= end + 4 + length { break; }
                assert!(count > 0);
            }
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(reply.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        }).collect()
        });
        (address, handle)
    }

    async fn finished(service: &SourceService, id: &str) -> codexflow_domain::SummaryRun {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.summary_run(id).unwrap().unwrap();
                if !matches!(
                    run.state,
                    SummaryRunState::Running | SummaryRunState::Cancelling
                ) {
                    break run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn summary_uses_text_service_cache_and_keeps_old_result_on_failure() {
        let (address, requests) = server(vec![
            r#"{"model":"actual-v1","choices":[{"message":{"content":"{\"goal\":\"验证目标\",\"activity\":\"执行检查\",\"outcome\":\"完成\",\"decisions\":\"保留方案\",\"issues\":\"无\",\"evidenceIds\":[\"item:turn-1:item-1\"]}"}}]}"#,
            r#"{"model":"actual-v2","choices":[{"message":{"content":"无效总结"}}]}"#,
        ]);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codexflow-text-summary-{nonce}"));
        let service = Arc::new(
            SourceService::with_credential_stores(
                root.clone(),
                Arc::new(EmptyCredentials),
                Arc::new(MemoryCredentials::default()),
            )
            .unwrap(),
        );
        service
            .sessions
            .save_collection(
                &[ThreadMetadata {
                    id: "thread-1".into(),
                    session_id: "session-1".into(),
                    title: Some("合成会话".into()),
                    preview: "验证目标".into(),
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
                    turns_complete: false,
                    items_complete: false,
                    missing_from_source: false,
                    content_complete: false,
                    read_error: None,
                    observed_at_unix_ms: 3,
                }],
                &[],
            )
            .unwrap();
        service
            .sessions
            .save_history(&HistorySnapshot {
                coverage: HistoryCoverage {
                    thread_id: "thread-1".into(),
                    source_updated_at: 2,
                    attempted_at_unix_ms: 3,
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
                    thread_id: "thread-1".into(),
                    id: "turn-1".into(),
                    ordinal: 0,
                    status: "completed".into(),
                    started_at_unix_ms: None,
                    completed_at_unix_ms: None,
                    duration_ms: None,
                    time_error: None,
                    source_updated_at: 2,
                    content_version: "turn-v1".into(),
                }],
                items: vec![HistoryItem {
                    thread_id: "thread-1".into(),
                    turn_id: "turn-1".into(),
                    id: "item-1".into(),
                    ordinal: 0,
                    source_type: "userMessage".into(),
                    supported: true,
                    text: Some("合成输入".into()),
                    command: None,
                    cwd: None,
                    output: None,
                    exit_code: None,
                    status: None,
                    changes: vec![],
                    source_updated_at: 2,
                    content_version: "item-v1".into(),
                }],
            })
            .unwrap();
        service
            .save_text(
                address.clone(),
                "requested-v1".into(),
                Some("test-secret-key".into()),
            )
            .await
            .unwrap();
        let preview = service.summary_preview("thread-1").await.unwrap();
        assert!(preview.content_available);
        assert_eq!(preview.character_limit, 40_000);
        assert_eq!(preview.included_messages, 1);
        let run = service
            .start_thread_summary("thread-1".into())
            .await
            .unwrap();
        assert_eq!(
            finished(&service, &run.id).await.state,
            SummaryRunState::Complete
        );
        let cached = service.summary_preview("thread-1").await.unwrap();
        assert!(cached.cache_current);
        assert_eq!(
            cached.cached_summary.as_ref().unwrap().content.goal,
            "验证目标"
        );
        assert_eq!(
            cached
                .cached_summary
                .as_ref()
                .unwrap()
                .service_base_url
                .as_deref(),
            Some(address.as_str())
        );
        service
            .save_text(
                address.clone(),
                "requested-v1".into(),
                Some("rotated-secret-key".into()),
            )
            .await
            .unwrap();
        assert!(
            service
                .summary_preview("thread-1")
                .await
                .unwrap()
                .cache_current
        );
        assert!(
            service
                .start_thread_summary("thread-1".into())
                .await
                .unwrap()
                .reused_cache
        );
        service
            .save_text(address, "requested-v2".into(), None)
            .await
            .unwrap();
        assert!(
            !service
                .summary_preview("thread-1")
                .await
                .unwrap()
                .cache_current
        );
        let failed = service
            .start_thread_summary("thread-1".into())
            .await
            .unwrap();
        assert_eq!(
            finished(&service, &failed.id).await.state,
            SummaryRunState::Failed
        );
        assert_eq!(
            service
                .sessions
                .summary("thread-1")
                .unwrap()
                .unwrap()
                .content
                .goal,
            "验证目标"
        );
        let requests = requests.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| !request
            .split_once("\r\n\r\n")
            .unwrap()
            .1
            .contains("secret-key")));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn validation_sends_only_fixed_synthetic_material() {
        let (address, requests) = server(vec![
            r#"{"model":"actual-model","choices":[{"message":{"content":"{\"ok\":true}"}}]}"#,
        ]);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codexflow-text-validation-{nonce}"));
        let service = SourceService::with_credential_stores(
            root.clone(),
            Arc::new(EmptyCredentials),
            Arc::new(MemoryCredentials::default()),
        )
        .unwrap();
        service
            .save_text(
                address,
                "requested-model".into(),
                Some("test-secret-key".into()),
            )
            .await
            .unwrap();
        let result = service.validate_text().await.unwrap();
        assert_eq!(result.actual_model, "actual-model");
        let request = requests.join().unwrap().remove(0);
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert!(body.contains("合成会话甲"));
        assert!(!body.contains("test-secret-key"));
        assert!(!body.contains("thread-1"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn late_summary_response_is_rejected_after_cancel_or_source_change() {
        for change_source in [false, true] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = format!("http://{}/v1", listener.local_addr().unwrap());
            let received = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let signal = Arc::clone(&received);
            let remote = thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut buffer = [0u8; 8192];
                let _ = socket.read(&mut buffer).unwrap();
                signal.store(true, std::sync::atomic::Ordering::SeqCst);
                thread::sleep(Duration::from_millis(250));
                let body = r#"{"model":"actual","choices":[{"message":{"content":"{\"goal\":\"迟到\",\"activity\":\"执行\",\"outcome\":\"完成\",\"decisions\":\"无\",\"issues\":\"无\",\"evidenceIds\":[\"item:turn-1:item-1\"]}"}}]}"#;
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.write_all(reply.as_bytes());
            });
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("codexflow-text-late-{nonce}-{change_source}"));
            let service = Arc::new(
                SourceService::with_credential_stores(
                    root.clone(),
                    Arc::new(EmptyCredentials),
                    Arc::new(MemoryCredentials::default()),
                )
                .unwrap(),
            );
            service
                .sessions
                .save_collection(
                    &[ThreadMetadata {
                        id: "thread-1".into(),
                        session_id: "session-1".into(),
                        title: None,
                        preview: "合成".into(),
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
                        turns_complete: false,
                        items_complete: false,
                        missing_from_source: false,
                        content_complete: false,
                        read_error: None,
                        observed_at_unix_ms: 3,
                    }],
                    &[],
                )
                .unwrap();
            service
                .sessions
                .save_history(&HistorySnapshot {
                    coverage: HistoryCoverage {
                        thread_id: "thread-1".into(),
                        source_updated_at: 2,
                        attempted_at_unix_ms: 3,
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
                        thread_id: "thread-1".into(),
                        id: "turn-1".into(),
                        ordinal: 0,
                        status: "completed".into(),
                        started_at_unix_ms: None,
                        completed_at_unix_ms: None,
                        duration_ms: None,
                        time_error: None,
                        source_updated_at: 2,
                        content_version: "turn-v1".into(),
                    }],
                    items: vec![HistoryItem {
                        thread_id: "thread-1".into(),
                        turn_id: "turn-1".into(),
                        id: "item-1".into(),
                        ordinal: 0,
                        source_type: "userMessage".into(),
                        supported: true,
                        text: Some("合成输入".into()),
                        command: None,
                        cwd: None,
                        output: None,
                        exit_code: None,
                        status: None,
                        changes: vec![],
                        source_updated_at: 2,
                        content_version: "item-v1".into(),
                    }],
                })
                .unwrap();
            service
                .save_text(address, "requested".into(), Some("synthetic-key".into()))
                .await
                .unwrap();
            let run = service
                .start_thread_summary("thread-1".into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while !received.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if change_source {
                let mut changed = service.sessions.thread("thread-1").unwrap().unwrap();
                changed.updated_at = 3;
                service.sessions.save_collection(&[changed], &[]).unwrap();
            } else {
                service.cancel_summary_run(&run.id).unwrap();
            }
            assert_eq!(
                finished(&service, &run.id).await.state,
                if change_source {
                    SummaryRunState::Failed
                } else {
                    SummaryRunState::Cancelled
                }
            );
            assert!(service.sessions.summary("thread-1").unwrap().is_none());
            remote.join().unwrap();
            let _ = fs::remove_dir_all(root);
        }
    }
}
