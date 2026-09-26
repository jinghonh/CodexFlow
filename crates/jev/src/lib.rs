//! TypeSafe 原生 HTTP 与系统凭据边界。公开结果不含密钥或原始响应。
use codexflow_domain::{
    AppError, ErrorCode, JevChoiceAnswer, JevConnectionResult, JevInferenceResult,
};
use reqwest::{header, redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use url::{Host, Url};

mod relation;
pub use relation::{JevRelationAnalyzer, RELATION_RULES_VERSION};

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

fn synthetic_inference_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "state": {"ticket": "合成工单 A", "resolution": "已由小组甲处理"},
        "model": model,
        "questions": {"classification": {
            "type": "choice",
            "instructions": "这条合成工单是否已处理？",
            "criteria": {"resolved": "已处理", "unresolved": "尚未处理"}
        }}
    })
}

fn error(code: ErrorCode, message: &'static str, retryable: bool) -> AppError {
    AppError::jev(code, message, retryable)
}

pub fn normalize_base_url(input: &str) -> Result<String, AppError> {
    let parsed = Url::parse(input.trim()).map_err(|_| invalid_address())?;
    let local = match parsed.host().ok_or_else(invalid_address)? {
        Host::Domain(name) => name == "localhost",
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    };
    if (parsed.scheme() != "https" && !(parsed.scheme() == "http" && local))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid_address());
    }
    let path = parsed.path().trim_end_matches('/').to_owned();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.iter().any(|s| s.eq_ignore_ascii_case("v1"))
        || path.contains('%')
        || path.contains('\\')
    {
        return Err(invalid_address());
    }
    let mut result = parsed;
    result.set_path(&path);
    Ok(result.as_str().trim_end_matches('/').to_owned())
}

fn invalid_address() -> AppError {
    error(
        ErrorCode::JevInvalidAddress,
        "请填写 HTTPS 服务根地址；本机测试可用回环 HTTP。不要包含 /v1、端点、账号、查询或片段。",
        false,
    )
}

pub struct Credential {
    pub base_url: String,
    pub key: String,
}

pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<Credential>, AppError>;
    fn save(&self, credential: &Credential) -> Result<(), AppError>;
    fn delete(&self) -> Result<(), AppError>;
}

pub fn system_credentials() -> Arc<dyn CredentialStore> {
    Arc::new(SystemCredentialStore {
        service: credential_service(b"dev.codexflow.desktop.jev"),
    })
}

pub fn system_text_credentials() -> Arc<dyn CredentialStore> {
    Arc::new(SystemCredentialStore {
        service: credential_service(b"dev.codexflow.desktop.text"),
    })
}

pub fn system_embedding_credentials() -> Arc<dyn CredentialStore> {
    Arc::new(SystemCredentialStore {
        service: credential_service(b"dev.codexflow.desktop.embedding"),
    })
}

pub struct SystemCredentialStore {
    service: Vec<u8>,
}

fn credential_service(service: &[u8]) -> Vec<u8> {
    let mut target = service.to_vec();
    if let Some(namespace) = std::env::var_os("CODEXFLOW_CREDENTIAL_NAMESPACE") {
        let namespace = namespace.to_string_lossy();
        if !namespace.is_empty() {
            target.push(b'.');
            target.extend(namespace.bytes().map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                    byte
                } else {
                    b'_'
                }
            }));
        }
    }
    target
}

impl CredentialStore for SystemCredentialStore {
    fn load(&self) -> Result<Option<Credential>, AppError> {
        let bytes = keychain::load(&self.service)?;
        bytes
            .map(|bytes| {
                let record: KeychainRecord =
                    serde_json::from_slice(&bytes).map_err(|_| credential_error())?;
                Ok(Credential {
                    base_url: record.base_url.to_owned(),
                    key: record.key.to_owned(),
                })
            })
            .transpose()
    }

    fn save(&self, credential: &Credential) -> Result<(), AppError> {
        let bytes = serde_json::to_vec(&KeychainRecord {
            base_url: &credential.base_url,
            key: &credential.key,
        })
        .map_err(|_| credential_error())?;
        keychain::save(&self.service, &bytes)
    }

    fn delete(&self) -> Result<(), AppError> {
        keychain::delete(&self.service)
    }
}

#[derive(serde::Serialize, Deserialize)]
struct KeychainRecord<'a> {
    base_url: &'a str,
    key: &'a str,
}

fn credential_error() -> AppError {
    error(
        ErrorCode::JevCredentialFailed,
        credential_error_message(),
        true,
    )
}

#[cfg(target_os = "macos")]
fn credential_error_message() -> &'static str {
    "无法访问 macOS 钥匙串。请解锁钥匙串并允许应用访问后重试。"
}

#[cfg(target_os = "windows")]
fn credential_error_message() -> &'static str {
    "无法访问 Windows 凭据管理器。请检查当前用户的系统凭据状态后重试。"
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn credential_error_message() -> &'static str {
    "当前平台没有可用的系统凭据库。凭据未保存，请在受支持的平台上重试。"
}

pub struct JevClient {
    client: Client,
}

impl JevClient {
    pub fn synthetic_request_characters(model: &str) -> usize {
        synthetic_inference_body(model).to_string().chars().count()
    }

    pub fn new() -> Result<Self, AppError> {
        Self::with_timeout(Duration::from_secs(180))
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self, AppError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| {
                error(
                    ErrorCode::JevConnectionFailed,
                    "无法初始化 Jev 连接。",
                    true,
                )
            })?;
        Ok(Self { client })
    }

    pub async fn check_connection(
        &self,
        credential: &Credential,
        model: &str,
    ) -> Result<JevConnectionResult, AppError> {
        reject_secret_in_model(credential, model)?;
        let url = format!("{}/v1/models", normalize_base_url(&credential.base_url)?);
        let response = self
            .client
            .get(url)
            .bearer_auth(&credential.key)
            .send()
            .await
            .map_err(network_error)?;
        let response = success(response).await?;
        let body: ModelsResponse = response.json().await.map_err(|_| protocol_error())?;
        if body
            .models
            .iter()
            .any(|entry| entry.name.trim().is_empty() || entry.name.contains(&credential.key))
        {
            return Err(protocol_error());
        }
        Ok(JevConnectionResult {
            models: body.models.into_iter().map(|entry| entry.name).collect(),
            requested_model: model.to_owned(),
        })
    }

    pub async fn test_inference(
        &self,
        credential: &Credential,
        model: &str,
    ) -> Result<JevInferenceResult, AppError> {
        reject_secret_in_model(credential, model)?;
        let url = format!("{}/v1/systemone", normalize_base_url(&credential.base_url)?);
        let body = synthetic_inference_body(model);
        let response = self
            .client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .bearer_auth(&credential.key)
            .json(&body)
            .send()
            .await
            .map_err(network_error)?;
        let response = success(response).await?;
        let body: InferenceResponse = response.json().await.map_err(|_| protocol_error())?;
        let answer = body
            .answers
            .get("classification")
            .ok_or_else(protocol_error)?;
        let resolved = answer.probabilities.get("resolved");
        let unresolved = answer.probabilities.get("unresolved");
        let valid_distribution = match (resolved, unresolved) {
            (Some(resolved), Some(unresolved)) => {
                answer.probabilities.len() == 2
                    && (0.0..=1.0).contains(resolved)
                    && (0.0..=1.0).contains(unresolved)
                    && (resolved + unresolved - 1.0).abs() <= 0.01
                    && (answer.choice != "resolved" || resolved >= unresolved)
                    && (answer.choice != "unresolved" || unresolved >= resolved)
            }
            _ => false,
        };
        if body.model.trim().is_empty()
            || body.model.contains(&credential.key)
            || answer.kind != "choice"
            || !matches!(answer.choice.as_str(), "resolved" | "unresolved")
            || !(0.0..=1.0).contains(&answer.confidence)
            || !valid_distribution
        {
            return Err(protocol_error());
        }
        Ok(JevInferenceResult {
            requested_model: model.to_owned(),
            actual_model: body.model,
            answer: JevChoiceAnswer {
                choice: answer.choice.clone(),
                confidence: answer.confidence,
                probabilities: answer.probabilities.clone(),
            },
            input_tokens: body.usage.input_tokens,
            output_tokens: body.usage.output_tokens,
        })
    }
}

#[derive(Deserialize)]
struct ModelsResponse {
    models: Vec<ModelEntry>,
}
#[derive(Deserialize)]
struct ModelEntry {
    name: String,
}
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}
#[derive(Deserialize)]
struct ChoiceAnswer {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    confidence: f64,
    probabilities: std::collections::BTreeMap<String, f64>,
}
#[derive(Deserialize)]
struct InferenceResponse {
    model: String,
    answers: std::collections::HashMap<String, ChoiceAnswer>,
    usage: Usage,
}

fn network_error(err: reqwest::Error) -> AppError {
    if err.is_timeout() {
        error(ErrorCode::JevTimeout, "Jev 请求超时，请稍后重试。", true)
    } else {
        error(
            ErrorCode::JevConnectionFailed,
            "无法连接 Jev 服务。请检查网络、TLS 证书和服务地址。",
            true,
        )
    }
}

fn protocol_error() -> AppError {
    error(
        ErrorCode::JevProtocolInvalid,
        "Jev 返回的数据不符合 TypeSafe 协议。",
        false,
    )
}

fn reject_secret_in_model(credential: &Credential, model: &str) -> Result<(), AppError> {
    if credential.key.is_empty() || model.contains(&credential.key) {
        Err(protocol_error())
    } else {
        Ok(())
    }
}

fn retry_after_ms(headers: &header::HeaderMap, now: SystemTime) -> Option<u64> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .parse::<u64>()
                .ok()
                .and_then(|seconds| seconds.checked_mul(1000))
                .or_else(|| {
                    httpdate::parse_http_date(value).ok().and_then(|date| {
                        u64::try_from(date.duration_since(now).unwrap_or_default().as_millis()).ok()
                    })
                })
        })
}

async fn success(response: reqwest::Response) -> Result<reqwest::Response, AppError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after_ms = retry_after_ms(response.headers(), SystemTime::now());
    // 只读取结构化错误码；服务返回的自由文本可能包含请求材料或凭据。
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("error")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    let quota = matches!(
        code,
        "insufficient_quota" | "quota_exceeded" | "billing_limit_exceeded"
    );
    let model = matches!(
        code,
        "model_not_found" | "unsupported_model" | "model_unsupported"
    );
    let mut result = if quota || status == StatusCode::PAYMENT_REQUIRED {
        error(
            ErrorCode::JevQuotaExceeded,
            "Jev 额度不足，请检查 TypeSafe 账户。",
            false,
        )
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        error(
            ErrorCode::JevAuthenticationFailed,
            "Jev 认证失败，请检查 API Key。",
            false,
        )
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        error(
            ErrorCode::JevRateLimited,
            "Jev 请求受到限流，请稍后重试。",
            true,
        )
    } else if status.as_u16() == 529 {
        error(
            ErrorCode::JevOverloaded,
            "Jev 服务暂时过载，请稍后重试。",
            true,
        )
    } else if model {
        error(
            ErrorCode::JevModelUnsupported,
            "所选 Jev 模型不可用，请检查模型 ID。",
            false,
        )
    } else if status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST {
        error(
            ErrorCode::JevInvalidRequest,
            "Jev 请求无效，请检查模型和服务配置。",
            false,
        )
    } else {
        protocol_error()
    };
    if matches!(
        result.code,
        ErrorCode::JevRateLimited | ErrorCode::JevOverloaded
    ) {
        result.retry_after_ms = retry_after_ms;
    }
    Err(result)
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::{credential_error, AppError};
    use std::ffi::c_void;
    const ACCOUNT: &[u8] = b"global";
    const NOT_FOUND: i32 = -25300;
    type Item = *mut c_void;
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainFindGenericPassword(
            keychain: Item,
            service_len: u32,
            service: *const u8,
            account_len: u32,
            account: *const u8,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut Item,
        ) -> i32;
        fn SecKeychainItemFreeContent(attrs: Item, data: *mut c_void) -> i32;
        fn SecKeychainItemDelete(item: Item) -> i32;
        fn SecKeychainItemModifyAttributesAndData(
            item: Item,
            attrs: Item,
            password_len: u32,
            password: *const u8,
        ) -> i32;
        fn SecKeychainAddGenericPassword(
            keychain: Item,
            service_len: u32,
            service: *const u8,
            account_len: u32,
            account: *const u8,
            password_len: u32,
            password: *const u8,
            item: *mut Item,
        ) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: Item);
    }

    fn find(
        service: &[u8],
        with_password: bool,
    ) -> Result<Option<(Item, Option<Vec<u8>>)>, AppError> {
        let mut item: Item = std::ptr::null_mut();
        let mut len = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            SecKeychainFindGenericPassword(
                std::ptr::null_mut(),
                service.len() as u32,
                service.as_ptr(),
                ACCOUNT.len() as u32,
                ACCOUNT.as_ptr(),
                if with_password {
                    &mut len
                } else {
                    std::ptr::null_mut()
                },
                if with_password {
                    &mut data
                } else {
                    std::ptr::null_mut()
                },
                &mut item,
            )
        };
        if status == NOT_FOUND {
            return Ok(None);
        }
        if status != 0 {
            return Err(credential_error());
        }
        let bytes = if with_password {
            let value = if data.is_null() {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec() }
            };
            unsafe {
                SecKeychainItemFreeContent(std::ptr::null_mut(), data);
            }
            Some(value)
        } else {
            None
        };
        Ok(Some((item, bytes)))
    }
    pub fn load(service: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        let found = find(service, true)?;
        Ok(found.map(|(item, bytes)| {
            unsafe {
                CFRelease(item);
            }
            bytes.expect("password requested")
        }))
    }
    pub fn delete(service: &[u8]) -> Result<(), AppError> {
        if let Some((item, _)) = find(service, false)? {
            let status = unsafe { SecKeychainItemDelete(item) };
            unsafe {
                CFRelease(item);
            }
            if status != 0 {
                return Err(credential_error());
            }
        }
        Ok(())
    }
    pub fn save(service: &[u8], bytes: &[u8]) -> Result<(), AppError> {
        let status = if let Some((item, _)) = find(service, false)? {
            let status = unsafe {
                SecKeychainItemModifyAttributesAndData(
                    item,
                    std::ptr::null_mut(),
                    bytes.len() as u32,
                    bytes.as_ptr(),
                )
            };
            unsafe {
                CFRelease(item);
            }
            status
        } else {
            unsafe {
                SecKeychainAddGenericPassword(
                    std::ptr::null_mut(),
                    service.len() as u32,
                    service.as_ptr(),
                    ACCOUNT.len() as u32,
                    ACCOUNT.as_ptr(),
                    bytes.len() as u32,
                    bytes.as_ptr(),
                    std::ptr::null_mut(),
                )
            }
        };
        if status != 0 {
            return Err(credential_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn retry_after_accepts_seconds_and_http_date_with_fixed_clock() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("7"));
        assert_eq!(retry_after_ms(&headers, now), Some(7_000));
        let date = httpdate::fmt_http_date(now + Duration::from_secs(12));
        headers.insert(header::RETRY_AFTER, date.parse().unwrap());
        assert_eq!(retry_after_ms(&headers, now), Some(12_000));
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_static("invalid"),
        );
        assert_eq!(retry_after_ms(&headers, now), None);
    }

    fn server(status: u16, body: &'static str) -> (String, thread::JoinHandle<String>) {
        server_with_headers(status, body, String::new())
    }

    fn server_with_headers(
        status: u16,
        body: &'static str,
        extra_headers: String,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let size = stream.read(&mut buf).unwrap();
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..size]);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (url, handle)
    }

    #[test]
    fn base_url_requires_a_service_root() {
        assert_eq!(
            normalize_base_url("https://api.typesafe.ai/gateway///").unwrap(),
            "https://api.typesafe.ai/gateway"
        );
        assert!(normalize_base_url("http://example.com").is_err());
        for input in [
            "https://api.typesafe.ai/v1",
            "https://api.typesafe.ai/v1/models",
            "https://api.typesafe.ai/v1/systemone",
            "https://user:pass@api.typesafe.ai",
            "https://api.typesafe.ai?token=abc",
            "https://api.typesafe.ai/#part",
        ] {
            assert!(normalize_base_url(input).is_err());
        }
        assert!(normalize_base_url("http://127.0.0.1:4242/bridge").is_ok());
        assert!(normalize_base_url("http://[::1]:4242/bridge").is_ok());
    }

    #[tokio::test]
    async fn native_endpoints_and_bearer_header_work_with_synthetic_data() {
        let client = JevClient::new().unwrap();
        let (base, request) = server(200, r#"{"models":[{"name":"jev-latest"}]}"#);
        let credential = Credential {
            base_url: base,
            key: "synthetic-only-key".into(),
        };
        let connected = client
            .check_connection(&credential, "jev-1.13.0")
            .await
            .unwrap();
        assert_eq!(connected.models, vec!["jev-latest"]);
        assert_eq!(connected.requested_model, "jev-1.13.0");
        let request = request.join().unwrap();
        assert!(request.starts_with("GET /v1/models "));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-only-key"));

        let (base, request) = server(
            200,
            r#"{"model":"jev-1.13.0","answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,"probabilities":{"resolved":0.9,"unresolved":0.1}}},"usage":{"input_tokens":42,"output_tokens":3}}"#,
        );
        let credential = Credential {
            base_url: base,
            key: "synthetic-only-key".into(),
        };
        let inferred = client
            .test_inference(&credential, "jev-latest")
            .await
            .unwrap();
        assert_eq!(inferred.actual_model, "jev-1.13.0");
        assert_eq!(inferred.input_tokens, 42);
        assert_eq!(inferred.answer.choice, "resolved");
        assert_eq!(inferred.answer.confidence, 0.9);
        assert_eq!(inferred.answer.probabilities["resolved"], 0.9);
        assert_eq!(inferred.answer.probabilities["unresolved"], 0.1);
        let request = request.join().unwrap();
        assert!(request.starts_with("POST /v1/systemone "));
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["classification"]["type"], "choice");
        assert_eq!(body["state"]["ticket"], "合成工单 A");
    }

    #[tokio::test]
    async fn http_errors_keep_rate_limits_distinct_from_quota() {
        let client = JevClient::new().unwrap();
        let cases = [
            (401, "{}", ErrorCode::JevAuthenticationFailed),
            (422, "{}", ErrorCode::JevInvalidRequest),
            (429, "{}", ErrorCode::JevRateLimited),
            (529, "{}", ErrorCode::JevOverloaded),
            (
                429,
                r#"{"code":"insufficient_quota"}"#,
                ErrorCode::JevQuotaExceeded,
            ),
            (
                400,
                r#"{"error":{"code":"model_not_found"}}"#,
                ErrorCode::JevModelUnsupported,
            ),
        ];
        for (status, body, expected) in cases {
            let (base, request) = server(status, body);
            let credential = Credential {
                base_url: base,
                key: "synthetic-only-key".into(),
            };
            let actual = client
                .check_connection(&credential, "jev-latest")
                .await
                .unwrap_err();
            assert_eq!(
                std::mem::discriminant(&actual.code),
                std::mem::discriminant(&expected)
            );
            request.join().unwrap();
        }
    }

    #[tokio::test]
    async fn echoed_credential_never_reaches_connection_or_inference_results() {
        let client = JevClient::new().unwrap();
        let (base, request) = server(200, r#"{"models":[{"name":"jev-synthetic-only-key"}]}"#);
        let credential = Credential {
            base_url: base,
            key: "synthetic-only-key".into(),
        };
        let result = client.check_connection(&credential, "jev-latest").await;
        assert!(result.is_err(), "回显密钥的模型列表必须被拒绝");
        let error = result.err().unwrap();
        assert!(matches!(error.code, ErrorCode::JevProtocolInvalid));
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("synthetic-only-key"));
        request.join().unwrap();

        let (base, request) = server(
            200,
            r#"{"model":"jev-synthetic-only-key","answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,"probabilities":{"resolved":0.9,"unresolved":0.1}}},"usage":{"input_tokens":42,"output_tokens":3}}"#,
        );
        let credential = Credential {
            base_url: base,
            key: "synthetic-only-key".into(),
        };
        let result = client.test_inference(&credential, "jev-latest").await;
        assert!(result.is_err(), "回显密钥的实际模型必须被拒绝");
        let error = result.err().unwrap();
        assert!(matches!(error.code, ErrorCode::JevProtocolInvalid));
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("synthetic-only-key"));
        request.join().unwrap();
    }

    #[tokio::test]
    async fn synthetic_choice_rejects_wrong_probability_keys_and_sum() {
        let client = JevClient::new().unwrap();
        for body in [
            r#"{"model":"jev-1.13.0","answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,"probabilities":{"resolved":0.9,"other":0.1}}},"usage":{"input_tokens":42,"output_tokens":3}}"#,
            r#"{"model":"jev-1.13.0","answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,"probabilities":{"resolved":0.4,"unresolved":0.1}}},"usage":{"input_tokens":42,"output_tokens":3}}"#,
            r#"{"model":"jev-1.13.0","answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,"probabilities":{"resolved":0.1,"unresolved":0.9}}},"usage":{"input_tokens":42,"output_tokens":3}}"#,
        ] {
            let (base, request) = server(200, body);
            let credential = Credential {
                base_url: base,
                key: "synthetic-only-key".into(),
            };
            let result = client.test_inference(&credential, "jev-latest").await;
            assert!(result.is_err(), "无效概率分布必须被拒绝");
            let error = result.err().unwrap();
            assert!(matches!(error.code, ErrorCode::JevProtocolInvalid));
            request.join().unwrap();
        }
    }

    #[tokio::test]
    async fn authentication_is_not_forwarded_to_a_redirect_target() {
        let destination = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        destination.set_nonblocking(true).unwrap();
        let location = format!(
            "Location: http://{}/capture\r\n",
            destination.local_addr().unwrap()
        );
        let (base, request) = server_with_headers(302, "", location);
        let credential = Credential {
            base_url: base,
            key: "synthetic-only-key".into(),
        };
        let error = JevClient::new()
            .unwrap()
            .check_connection(&credential, "jev-latest")
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::JevProtocolInvalid));
        request.join().unwrap();
        assert!(matches!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }
}

#[cfg(target_os = "windows")]
mod keychain {
    use super::{credential_error, AppError};
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{
        Foundation::GetLastError,
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    };

    const ERROR_NOT_FOUND: u32 = 1168;
    const MAX_CREDENTIAL_BLOB_SIZE: usize = 2560;

    fn target_name(service: &[u8]) -> Result<Vec<u16>, AppError> {
        let service = std::str::from_utf8(service).map_err(|_| credential_error())?;
        let mut target: Vec<u16> = service.encode_utf16().collect();
        target.push(0);
        Ok(target)
    }

    struct CredentialGuard(*mut CREDENTIALW);

    impl Drop for CredentialGuard {
        fn drop(&mut self) {
            unsafe { CredFree(self.0.cast::<c_void>()) };
        }
    }

    pub fn load(service: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        let target = target_name(service)?;
        let mut credential = ptr::null_mut();
        let succeeded =
            unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
        if succeeded == 0 {
            return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                Ok(None)
            } else {
                Err(credential_error())
            };
        }

        let credential = CredentialGuard(credential);
        let value = unsafe { &*credential.0 };
        if value.CredentialBlobSize == 0 {
            return Ok(Some(Vec::new()));
        }
        if value.CredentialBlob.is_null() {
            return Err(credential_error());
        }
        Ok(Some(unsafe {
            std::slice::from_raw_parts(value.CredentialBlob, value.CredentialBlobSize as usize)
                .to_vec()
        }))
    }

    pub fn save(service: &[u8], bytes: &[u8]) -> Result<(), AppError> {
        if bytes.len() > MAX_CREDENTIAL_BLOB_SIZE {
            return Err(credential_error());
        }
        let target = target_name(service)?;
        let mut credential: CREDENTIALW = unsafe { std::mem::zeroed() };
        credential.Type = CRED_TYPE_GENERIC;
        credential.TargetName = target.as_ptr() as *mut u16;
        credential.CredentialBlobSize = bytes.len() as u32;
        credential.CredentialBlob = bytes.as_ptr() as *mut u8;
        credential.Persist = CRED_PERSIST_LOCAL_MACHINE;
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(credential_error());
        }
        Ok(())
    }

    pub fn delete(service: &[u8]) -> Result<(), AppError> {
        let target = target_name(service)?;
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0
            && unsafe { GetLastError() } != ERROR_NOT_FOUND
        {
            return Err(credential_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::time::{SystemTime, UNIX_EPOCH};

        #[test]
        fn credential_manager_round_trip_uses_an_isolated_target() {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let service = format!("dev.codexflow.test.{}.{}", std::process::id(), nonce);
            delete(service.as_bytes()).unwrap();
            assert_eq!(load(service.as_bytes()).unwrap(), None);
            save(service.as_bytes(), b"synthetic-only-credential").unwrap();
            assert_eq!(
                load(service.as_bytes()).unwrap().as_deref(),
                Some(&b"synthetic-only-credential"[..])
            );
            delete(service.as_bytes()).unwrap();
            assert_eq!(load(service.as_bytes()).unwrap(), None);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod keychain {
    use super::{credential_error, AppError};
    pub fn load(_: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        Err(credential_error())
    }
    pub fn save(_: &[u8], _: &[u8]) -> Result<(), AppError> {
        Err(credential_error())
    }
    pub fn delete(_: &[u8]) -> Result<(), AppError> {
        Err(credential_error())
    }
}
