use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};

mod timeline;
pub use timeline::{
    build_project_timeline, LastActivityBasis, ProjectTimeline, TimelineQuality, TimelineThread,
    TimelineTurn, TurnTimeState,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub cache_preserved: bool,
    pub backend: String,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("AppError", 7)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("message", &self.message)?;
        state.serialize_field("retryable", &self.retryable)?;
        state.serialize_field("cachePreserved", &self.cache_preserved)?;
        state.serialize_field("backend", &self.backend)?;
        state.serialize_field("retryAfterMs", &self.retry_after_ms)?;
        state.serialize_field("nextStep", self.next_step())?;
        state.end()
    }
}

impl AppError {
    pub fn next_step(&self) -> &'static str {
        match self.code {
            ErrorCode::StorageFailed => "检查应用数据目录权限、可用空间和其他写入进程，然后重试。",
            ErrorCode::MigrationFailed => {
                "保留数据库文件，修复存储问题后重启应用；若仍失败，请使用受支持的应用版本。"
            }
            ErrorCode::DatabaseTooNew => {
                "使用创建此数据库的较新版本应用打开；不要覆盖或删除原数据库。"
            }
            ErrorCode::ConcurrentModification => "刷新当前项目后重新提交修改。",
            ErrorCode::JevCredentialFailed => {
                "解锁钥匙串并允许应用访问；如密钥已删除，请重新填写 API Key。"
            }
            ErrorCode::TextCredentialFailed => "解锁钥匙串并允许应用访问，然后重试。",
            ErrorCode::TextNotConfigured => "填写文本生成服务地址、模型和当前地址的 API Key。",
            ErrorCode::TextInvalidAddress => "填写有效的 HTTPS 服务地址并重新保存。",
            ErrorCode::TextConnectionFailed | ErrorCode::TextProtocolInvalid => {
                "检查文本服务地址、协议和网络后重试。"
            }
            ErrorCode::JevNotConfigured => "为当前服务地址填写并保存 API Key。",
            ErrorCode::JevInvalidAddress => "填写有效的 HTTPS 服务根地址并重新保存。",
            ErrorCode::JevAuthenticationFailed | ErrorCode::AnalysisAuthenticationFailed => {
                "检查对应服务的凭据后重试。"
            }
            ErrorCode::JevQuotaExceeded | ErrorCode::AnalysisQuotaExceeded => {
                "检查对应服务的额度或账户状态后重试。"
            }
            ErrorCode::JevRateLimited
            | ErrorCode::JevOverloaded
            | ErrorCode::AnalysisOverloaded => "稍后重试；已保存的结果仍可查看。",
            ErrorCode::BinaryUnavailable
            | ErrorCode::SpawnFailed
            | ErrorCode::InitializeFailed
            | ErrorCode::ProtocolIncompatible
            | ErrorCode::ProcessExited => "检查或更换 Codex 二进制后重试，已有缓存仍可查看。",
            ErrorCode::SourceReadFailed => "检查 Codex 来源状态后重新刷新，已有缓存仍可查看。",
            ErrorCode::JevConnectionFailed | ErrorCode::JevTimeout => {
                "检查服务地址与网络连接后重试。"
            }
            ErrorCode::JevModelUnsupported | ErrorCode::AnalysisModelUnsupported => {
                "检查模型 ID 与服务支持情况后重试。"
            }
            ErrorCode::JevProtocolInvalid
            | ErrorCode::JevInvalidRequest
            | ErrorCode::AnalysisInvalidResult => "检查配置或结果详情，修正后重新运行。",
            ErrorCode::JevCancelled | ErrorCode::AnalysisCancelled => "需要时重新启动分析。",
            ErrorCode::ProjectResolutionFailed => "检查项目路径与工作区归属后重新选择项目。",
            ErrorCode::RefreshAlreadyRunning | ErrorCode::AnalysisAlreadyRunning => {
                "等待当前运行结束，或先取消当前运行。"
            }
            ErrorCode::RefreshNotFound | ErrorCode::AnalysisNotFound => "刷新运行状态后重试。",
            ErrorCode::AnalysisBudgetInvalid => "调整分析调用预算后重试。",
            ErrorCode::AnalysisConfigChanged => "刷新分析预览并使用当前配置重新运行。",
            ErrorCode::AnalysisTimeout | ErrorCode::AnalysisUnavailable => {
                "检查分析服务状态后重试，已有结果仍可查看。"
            }
        }
    }

    pub fn jev(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            cache_preserved: true,
            backend: "jev".into(),
            retry_after_ms: None,
        }
    }
    pub fn text(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            cache_preserved: true,
            backend: "text".into(),
            retry_after_ms: None,
        }
    }
    pub fn codex(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            cache_preserved: true,
            backend: "codex".into(),
            retry_after_ms: None,
        }
    }

    pub fn store(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::StorageFailed,
            message: message.into(),
            retryable: true,
            cache_preserved: true,
            backend: "store".into(),
            retry_after_ms: None,
        }
    }

    pub fn migration(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::MigrationFailed,
            message: message.into(),
            retryable: false,
            cache_preserved: true,
            backend: "store".into(),
            retry_after_ms: None,
        }
    }

    pub fn database_too_new() -> Self {
        Self {
            code: ErrorCode::DatabaseTooNew,
            message: "会话数据库来自更新版本的应用，当前版本无法安全打开。".into(),
            retryable: false,
            cache_preserved: true,
            backend: "store".into(),
            retry_after_ms: None,
        }
    }

    pub fn project(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::ProjectResolutionFailed,
            message: message.into(),
            retryable: true,
            cache_preserved: true,
            backend: "core".into(),
            retry_after_ms: None,
        }
    }

    pub fn conflict() -> Self {
        Self {
            code: ErrorCode::ConcurrentModification,
            message: "关系裁决已被更新，请刷新后重试。".into(),
            retryable: true,
            cache_preserved: true,
            backend: "store".into(),
            retry_after_ms: None,
        }
    }

    pub fn workstream_conflict() -> Self {
        Self {
            code: ErrorCode::ConcurrentModification,
            message: "工作流已被更新，请刷新后重试。".into(),
            retryable: true,
            cache_preserved: true,
            backend: "store".into(),
            retry_after_ms: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    BinaryUnavailable,
    SpawnFailed,
    InitializeFailed,
    ProtocolIncompatible,
    ProcessExited,
    SourceReadFailed,
    StorageFailed,
    JevInvalidAddress,
    JevNotConfigured,
    JevConnectionFailed,
    JevAuthenticationFailed,
    JevModelUnsupported,
    JevProtocolInvalid,
    JevRateLimited,
    JevOverloaded,
    JevQuotaExceeded,
    JevInvalidRequest,
    JevTimeout,
    JevCancelled,
    JevCredentialFailed,
    TextInvalidAddress,
    TextNotConfigured,
    TextCredentialFailed,
    TextConnectionFailed,
    TextProtocolInvalid,
    MigrationFailed,
    DatabaseTooNew,
    ProjectResolutionFailed,
    RefreshAlreadyRunning,
    RefreshNotFound,
    AnalysisUnavailable,
    AnalysisModelUnsupported,
    AnalysisAlreadyRunning,
    AnalysisNotFound,
    AnalysisInvalidResult,
    AnalysisCancelled,
    AnalysisBudgetInvalid,
    AnalysisConfigChanged,
    AnalysisTimeout,
    AnalysisAuthenticationFailed,
    AnalysisQuotaExceeded,
    AnalysisOverloaded,
    ConcurrentModification,
}

/// A stage is scheduled by core, while its transport remains in its adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisStage {
    Summary,
    Relation,
    EvidenceSelection,
    Naming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisStageSelection {
    pub summary: bool,
    /// Enables both candidate classification and any dependent evidence selection.
    pub relations: bool,
    pub naming: bool,
}

impl Default for AnalysisStageSelection {
    fn default() -> Self {
        // Preserve the former all-stages behavior for saved runs without this field.
        Self {
            summary: true,
            relations: true,
            naming: true,
        }
    }
}

impl AnalysisStageSelection {
    pub fn any_selected(self) -> bool {
        self.summary || self.relations || self.naming
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisLimits {
    pub call_limit: u32,
    pub concurrency_limit: u8,
    pub timeout_seconds: u64,
    pub retry_limit: u8,
    pub input_character_limit: usize,
}

impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            call_limit: 100,
            concurrency_limit: 2,
            timeout_seconds: 180,
            retry_limit: 2,
            input_character_limit: 40_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisStagePlan {
    pub stage: AnalysisStage,
    pub service: String,
    pub model: String,
    pub send_scope: String,
    pub pending_items: u64,
    pub maximum_calls: u64,
    pub available: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisPreview {
    pub project_id: String,
    pub input_version: String,
    pub stages: Vec<AnalysisStagePlan>,
    pub cached_summaries: u64,
    pub unavailable_summaries: u64,
    pub maximum_candidates: u64,
    pub evidence_selection_call_limit: u64,
    pub pending_groups: Option<u64>,
    pub limits: AnalysisLimits,
    pub jev_configured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisRunState {
    Queued,
    Running,
    Cancelling,
    Cancelled,
    Paused,
    Complete,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisUnitState {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisUnit {
    pub id: String,
    pub stage: AnalysisStage,
    pub input_version: String,
    pub state: AnalysisUnitState,
    pub attempts: u32,
    pub active_summary_run_id: Option<String>,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub error: Option<AppError>,
    #[serde(default)]
    pub relation_classification: Option<JevRelationClassification>,
    #[serde(default)]
    pub relation_evidence_selection: Option<JevEvidenceSelection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRun {
    pub id: String,
    pub project_id: String,
    pub state: AnalysisRunState,
    pub pause_reason: Option<String>,
    pub input_version: String,
    pub codex_binary: Option<String>,
    #[serde(default)]
    pub codex_binary_fingerprint: Option<String>,
    pub codex_version: Option<String>,
    #[serde(rename = "textModel", alias = "codexModel")]
    pub codex_model: String,
    #[serde(default)]
    pub text_base_url: String,
    #[serde(default)]
    pub text_config_revision: u64,
    pub jev_base_url: String,
    pub jev_model: String,
    #[serde(default)]
    pub jev_rules_version: String,
    pub jev_config_revision: u64,
    #[serde(default)]
    pub jev_pinned_model: Option<String>,
    #[serde(default)]
    pub jev_probe_attempts: u32,
    pub limits: AnalysisLimits,
    #[serde(default)]
    pub stage_selection: AnalysisStageSelection,
    pub batch_number: u32,
    pub batch_calls: u32,
    pub total_calls: u32,
    pub total_questions: u32,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub processed: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub pending: u32,
    pub units: Vec<AnalysisUnit>,
    #[serde(default)]
    pub relations_planned: bool,
    #[serde(default)]
    pub names_planned: bool,
    #[serde(default)]
    pub relation_only: bool,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub interrupted: bool,
    pub error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMetadata {
    pub branch: Option<String>,
    pub sha: Option<String>,
    pub origin_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadMetadata {
    pub id: String,
    pub session_id: String,
    pub title: Option<String>,
    pub preview: String,
    pub cwd: String,
    pub project_id: Option<String>,
    pub source_kind: String,
    pub source_detail: Option<String>,
    pub thread_source: Option<String>,
    pub parent_thread_id: Option<String>,
    pub forked_from_id: Option<String>,
    pub git: Option<GitMetadata>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub metadata_complete: bool,
    #[serde(default)]
    pub turns_complete: bool,
    #[serde(default)]
    pub items_complete: bool,
    #[serde(default)]
    pub missing_from_source: bool,
    pub content_complete: bool,
    pub read_error: Option<String>,
    pub observed_at_unix_ms: i64,
}

/// Coverage describes the latest source read, independently of any older cached rows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCoverage {
    pub thread_id: String,
    pub source_updated_at: i64,
    pub attempted_at_unix_ms: i64,
    pub path: HistoryReadPath,
    pub turns_complete: bool,
    pub items_complete: bool,
    pub turn_pages: u32,
    pub item_pages: u32,
    pub loaded_turns: u64,
    pub loaded_items: u64,
    #[serde(default)]
    pub incompatible: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HistoryReadPath {
    None,
    Paginated,
    FullRead,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryTurn {
    pub thread_id: String,
    pub id: String,
    pub ordinal: u64,
    pub status: String,
    pub started_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub time_error: Option<String>,
    pub source_updated_at: i64,
    pub content_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFileChange {
    pub path: String,
    pub kind: String,
    pub diff: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    /// 条目身份由 (thread_id, turn_id, id) 组成；不同回合可以复用来源条目标识。
    pub thread_id: String,
    pub turn_id: String,
    pub id: String,
    pub ordinal: u64,
    pub source_type: String,
    pub supported: bool,
    pub text: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i64>,
    pub status: Option<String>,
    pub changes: Vec<HistoryFileChange>,
    pub source_updated_at: i64,
    pub content_version: String,
}

#[derive(Clone, Debug)]
pub struct HistorySnapshot {
    pub coverage: HistoryCoverage,
    pub turns: Vec<HistoryTurn>,
    pub items: Vec<HistoryItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryTurnPage {
    pub coverage: Option<HistoryCoverage>,
    pub turns: Vec<HistoryTurn>,
    pub total: u64,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemPage {
    pub coverage: Option<HistoryCoverage>,
    pub items: Vec<HistoryItem>,
    pub total: u64,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItemLocation {
    pub turn_id: String,
    pub turn_offset: u64,
    pub offset: u64,
}

/// 自动事实只陈述结构化来源条目记录的操作；结果未知仍是未知。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFact {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub kind: FactKind,
    pub subject: String,
    pub operation: String,
    pub outcome: FactOutcome,
    pub evidence_id: String,
    pub content_version: String,
    pub rule_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FactKind {
    File,
    Command,
    Branch,
    Artifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FactOutcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceField {
    Text,
    Command,
    Output,
    ChangePath,
    ChangeDiff,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEvidence {
    pub id: String,
    pub fact_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub field: EvidenceField,
    pub change_index: Option<u32>,
    pub excerpt: String,
    pub content_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ThreadSummaryContent {
    pub goal: String,
    pub activity: String,
    pub outcome: String,
    pub decisions: String,
    pub issues: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub thread_id: String,
    pub content: ThreadSummaryContent,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<ThreadSummaryEvidence>,
    pub model: String,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub service_base_url: Option<String>,
    #[serde(default)]
    pub binary_path: Option<String>,
    #[serde(default)]
    pub binary_fingerprint: Option<String>,
    #[serde(default)]
    pub binary_version: Option<String>,
    pub input_digest: String,
    pub source_updated_at: i64,
    #[serde(default)]
    pub history_generation: i64,
    pub created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummaryEvidence {
    pub id: String,
    pub turn_id: String,
    pub item_id: String,
    pub content_version: String,
    pub excerpt: String,
    pub is_fact: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryEvidenceCheck {
    pub id: String,
    pub state: EvidenceState,
    pub message: String,
    pub item_id: Option<String>,
    pub location: Option<HistoryItemLocation>,
    pub excerpt: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPreview {
    pub thread_id: String,
    #[serde(default)]
    pub service_base_url: Option<String>,
    pub model: String,
    pub character_limit: usize,
    pub character_count: usize,
    pub total_facts: usize,
    pub included_facts: usize,
    pub total_messages: usize,
    pub included_messages: usize,
    pub truncated: bool,
    pub turns_complete: bool,
    pub items_complete: bool,
    pub source_current: bool,
    #[serde(default)]
    pub read_error: Option<String>,
    pub content_available: bool,
    pub cached_summary: Option<ThreadSummary>,
    pub cache_current: bool,
    #[serde(default)]
    pub stale_reason: Option<String>,
    pub analysis_blocked_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryRunState {
    Running,
    Cancelling,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryRun {
    pub id: String,
    pub thread_id: String,
    pub state: SummaryRunState,
    pub model: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub temporary_thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub reused_cache: bool,
    pub error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactPage {
    pub facts: Vec<SourceFact>,
    pub total: u64,
    pub offset: u64,
    pub limit: u32,
    pub coverage: Option<HistoryCoverage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidencePage {
    pub evidence: Vec<SourceEvidence>,
    pub total: u64,
    pub offset: u64,
    pub limit: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceState {
    Valid,
    MissingThread,
    MissingTurn,
    MissingItem,
    MissingFact,
    WrongHierarchy,
    ExcerptMissing,
    StaleVersion,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceCheck {
    pub state: EvidenceState,
    pub message: String,
    pub location: Option<HistoryItemLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListScopeStatus {
    pub archived: bool,
    pub complete: bool,
    pub attempted_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionList {
    pub threads: Vec<ThreadMetadata>,
    pub scopes: Vec<ListScopeStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexRunState {
    Queued,
    Running,
    Complete,
    Partial,
    Failed,
    Cancelled,
}

impl IndexRunState {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Partial | Self::Failed | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRun {
    pub id: String,
    pub project_id: Option<String>,
    pub state: IndexRunState,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub pages_saved: u64,
    pub threads_seen: u64,
    pub error: Option<AppError>,
    pub interrupted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProject {
    pub id: String,
    pub name: String,
    pub root: String,
    pub git_common_dir: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadAttribution {
    pub thread_id: String,
    pub project_id: Option<String>,
    pub workspace_root: Option<String>,
    pub basis: String,
    pub detail: String,
    pub diagnostic: Option<String>,
    pub source_project_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributedThread {
    pub thread: ThreadMetadata,
    pub attribution: ThreadAttribution,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCatalog {
    pub projects: Vec<LocalProject>,
    pub selected_project_id: Option<String>,
    pub recent_project_ids: Vec<String>,
    pub unassigned: Vec<AttributedThread>,
    pub scopes: Vec<ListScopeStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSessions {
    pub project: LocalProject,
    pub workspaces: Vec<String>,
    pub threads: Vec<AttributedThread>,
    pub scopes: Vec<ListScopeStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObservedRelationKind {
    ForkedFrom,
    SubagentOf,
}

impl ObservedRelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ForkedFrom => "FORKED_FROM",
            Self::SubagentOf => "SUBAGENT_OF",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParentEndpoint {
    InProject,
    Missing,
    OutsideProject,
    Unassigned,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedRelation {
    pub id: String,
    pub project_id: String,
    pub from_thread_id: String,
    pub to_thread_id: String,
    pub kind: ObservedRelationKind,
    pub source: String,
    pub source_field: String,
    pub confidence: f64,
    pub parent_endpoint: ParentEndpoint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    pub reference_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDiagnostic {
    pub thread_id: String,
    pub source_field: String,
    pub referenced_thread_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGraph {
    pub project: LocalProject,
    pub nodes: Vec<GraphNode>,
    pub relations: Vec<ObservedRelation>,
    #[serde(default)]
    pub derived_relations: Vec<DerivedRelation>,
    #[serde(default)]
    pub inferred_relations: Vec<InferredRelation>,
    #[serde(default)]
    pub reviewed_relations: Vec<ReviewedInferredRelation>,
    #[serde(default)]
    pub inference_outcomes: Vec<InferredPairOutcome>,
    pub diagnostics: Vec<GraphDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workstream {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub members: Vec<String>,
    pub relation_ids: Vec<String>,
    pub algorithm_version: String,
    pub name_input_version: Option<String>,
    #[serde(default)]
    pub name_actual_model: Option<String>,
    #[serde(default)]
    pub name_service_base_url: Option<String>,
    #[serde(default)]
    pub name_requested_model: Option<String>,
    pub name_error: Option<String>,
    pub predecessor_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkstreams {
    pub project_id: String,
    pub workstreams: Vec<Workstream>,
    pub ungrouped_thread_ids: Vec<String>,
    pub cross_relation_ids: Vec<String>,
    /// Revision of the whole project workstream view, including automatic changes.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub manually_named_workstream_ids: Vec<String>,
    #[serde(default)]
    pub manually_assigned_thread_ids: Vec<String>,
}

/// 规则只记录来源事实的交集或引用，不表达语义因果关系。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DerivedRelationKind {
    SharedFile,
    SharedArtifact,
    ExplicitReference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateEvidence {
    pub id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub field: EvidenceField,
    pub change_index: Option<u32>,
    pub excerpt: String,
    pub content_version: String,
    pub fact_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidencePair {
    pub id: String,
    pub left: CandidateEvidence,
    pub right: CandidateEvidence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSample {
    pub left_available: u64,
    pub right_available: u64,
    pub combinations_available: u64,
    pub combinations_shown: u32,
    pub left_sampled: u32,
    pub right_sampled: u32,
    pub sampling_rule: String,
    pub pairs: Vec<EvidencePair>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateReason {
    pub signal: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationCandidate {
    pub id: String,
    pub left_thread_id: String,
    pub right_thread_id: String,
    pub score: i32,
    pub reasons: Vec<CandidateReason>,
    pub evidence: EvidenceSample,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePreview {
    pub project_id: String,
    /// Digest of project metadata and complete history revisions used to build this view.
    #[serde(default)]
    pub input_version: String,
    pub thread_count: u64,
    #[serde(default)]
    pub unavailable_threads: u64,
    pub neighbor_limit: u32,
    pub candidate_count: u64,
    pub candidates: Vec<RelationCandidate>,
    #[serde(default)]
    pub candidate_versions: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub stale_candidates: Vec<StaleCandidate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StaleCandidate {
    pub candidate: RelationCandidate,
    pub input_version: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedRelation {
    pub id: String,
    pub project_id: String,
    pub from_thread_id: String,
    pub to_thread_id: String,
    pub kind: DerivedRelationKind,
    pub source: String,
    pub basis: String,
    pub evidence: Vec<CandidateEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InferredRelationKind {
    Continues,
    Implements,
    Fixes,
    Validates,
    Investigates,
    AlternativeTo,
    Supersedes,
    MotivatedBy,
    Related,
}

impl InferredRelationKind {
    pub fn directed(self) -> bool {
        !matches!(self, Self::AlternativeTo | Self::Related)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continues => "CONTINUES",
            Self::Implements => "IMPLEMENTS",
            Self::Fixes => "FIXES",
            Self::Validates => "VALIDATES",
            Self::Investigates => "INVESTIGATES",
            Self::AlternativeTo => "ALTERNATIVE_TO",
            Self::Supersedes => "SUPERSEDES",
            Self::MotivatedBy => "MOTIVATED_BY",
            Self::Related => "RELATED",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationJudgment {
    Supported,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationChoice {
    pub key: String,
    pub kind: InferredRelationKind,
    pub from_thread_id: String,
    pub to_thread_id: String,
    pub judgment: RelationJudgment,
    pub answer: JevChoiceAnswer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevRelationClassification {
    pub requested_model: String,
    pub actual_model: String,
    pub choices: Vec<RelationChoice>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevEvidenceChoice {
    pub relation_key: String,
    pub pair_id: Option<String>,
    pub answer: JevChoiceAnswer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevEvidenceSelection {
    pub actual_model: String,
    pub choices: Vec<JevEvidenceChoice>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CausalTimeCheck {
    Verified,
    Unverifiable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredRelation {
    pub id: String,
    pub project_id: String,
    pub candidate_id: String,
    pub from_thread_id: String,
    pub to_thread_id: String,
    pub kind: InferredRelationKind,
    pub source: String,
    pub requested_model: String,
    pub actual_model: String,
    pub confidence: f64,
    pub probabilities: std::collections::BTreeMap<String, f64>,
    pub evidence_confidence: f64,
    pub evidence_probabilities: std::collections::BTreeMap<String, f64>,
    pub evidence: EvidencePair,
    pub time_check: CausalTimeCheck,
    pub explanation: String,
    pub input_version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserRelationDecision {
    Pending,
    Confirmed,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationReview {
    pub relation_id: String,
    pub project_id: String,
    pub decision: UserRelationDecision,
    pub revision: u64,
    pub confirmed_evidence_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedInferredRelation {
    #[serde(flatten)]
    pub relation: InferredRelation,
    pub evidence_version: String,
    pub evidence_valid: bool,
    #[serde(default)]
    pub stale_reason: Option<String>,
    pub review: RelationReview,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InferredPairOutcome {
    pub candidate_id: String,
    pub project_id: String,
    pub input_version: String,
    pub status: String,
    pub unknown_count: u32,
    #[serde(default)]
    pub decisions: Vec<RelationChoice>,
    #[serde(default)]
    pub jev_identity: Option<JevDecisionIdentity>,
    pub relations: Vec<InferredRelation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevDecisionIdentity {
    pub base_url: String,
    pub requested_model: String,
    pub actual_model: String,
    pub rules_version: String,
    #[serde(default)]
    pub input_character_limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    NotChecked,
    Connecting,
    Connected,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityState {
    Available,
    Unavailable,
    NotVerified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub state: CapabilityState,
    pub detail: String,
}

impl Capability {
    pub fn available(detail: impl Into<String>) -> Self {
        Self {
            state: CapabilityState::Available,
            detail: detail.into(),
        }
    }
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            state: CapabilityState::Unavailable,
            detail: detail.into(),
        }
    }
    pub fn unverified(detail: impl Into<String>) -> Self {
        Self {
            state: CapabilityState::NotVerified,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCapabilities {
    pub metadata: Capability,
    pub history: Capability,
    pub experimental_history: Capability,
    pub codex_summary: Capability,
    pub codex_naming: Capability,
}

impl Default for SourceCapabilities {
    fn default() -> Self {
        Self {
            metadata: Capability::unverified("尚未连接"),
            history: Capability::unverified("尚未连接"),
            experimental_history: Capability::unverified("尚未连接"),
            codex_summary: Capability::unverified("尚未连接"),
            codex_naming: Capability::unverified("尚未连接"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub selected_binary: Option<String>,
    pub resolved_binary: Option<String>,
    pub version: Option<String>,
    pub connection: ConnectionState,
    pub capabilities: SourceCapabilities,
    pub error: Option<AppError>,
    pub checked_at_unix_ms: Option<u128>,
}

impl SourceStatus {
    pub fn new(selected_binary: Option<String>) -> Self {
        Self {
            selected_binary,
            resolved_binary: None,
            version: None,
            connection: ConnectionState::NotChecked,
            capabilities: SourceCapabilities::default(),
            error: None,
            checked_at_unix_ms: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DisplayTheme {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub selected_binary: Option<String>,
    pub theme: DisplayTheme,
    #[serde(default)]
    pub jev: JevConfig,
    #[serde(default)]
    pub jev_revision: u64,
    #[serde(default)]
    pub text: TextConfig,
    #[serde(default)]
    pub text_revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextConfig {
    pub base_url: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStatus {
    pub config: TextConfig,
    pub credential_configured: bool,
    pub credential_error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextValidation {
    pub requested_model: String,
    pub actual_model: String,
    pub reply: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevConfig {
    pub base_url: String,
    pub model: String,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.typesafe.ai".into(),
            model: "jev-latest".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevStatus {
    pub config: JevConfig,
    pub credential_configured: bool,
    pub credential_error: Option<AppError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevConnectionResult {
    pub models: Vec<String>,
    pub requested_model: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevInferenceResult {
    pub requested_model: String,
    pub actual_model: String,
    pub answer: JevChoiceAnswer,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JevChoiceAnswer {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: std::collections::BTreeMap<String, f64>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            selected_binary: None,
            theme: DisplayTheme::System,
            jev: JevConfig::default(),
            jev_revision: 0,
            text: TextConfig::default(),
            text_revision: 0,
        }
    }
}
