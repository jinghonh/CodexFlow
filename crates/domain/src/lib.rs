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
    pub content_complete: bool,
    pub read_error: Option<String>,
    pub observed_at_unix_ms: i64,
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
