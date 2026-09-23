use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub cache_preserved: bool,
    pub backend: String,
}

impl AppError {
    pub fn jev(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            cache_preserved: true,
            backend: "jev".into(),
        }
    }
    pub fn codex(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            cache_preserved: true,
            backend: "codex".into(),
        }
    }

    pub fn store(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::StorageFailed,
            message: message.into(),
            retryable: true,
            cache_preserved: true,
            backend: "store".into(),
        }
    }

    pub fn migration(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::MigrationFailed,
            message: message.into(),
            retryable: false,
            cache_preserved: true,
            backend: "store".into(),
        }
    }

    pub fn project(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::ProjectResolutionFailed,
            message: message.into(),
            retryable: true,
            cache_preserved: true,
            backend: "core".into(),
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
    MigrationFailed,
    ProjectResolutionFailed,
    RefreshAlreadyRunning,
    RefreshNotFound,
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
    pub diagnostics: Vec<GraphDiagnostic>,
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
        }
    }
}
