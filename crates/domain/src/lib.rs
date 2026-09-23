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
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            selected_binary: None,
            theme: DisplayTheme::System,
        }
    }
}
