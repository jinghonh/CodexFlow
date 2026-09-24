//! OpenAI compatible text generation. Only validated output crosses this boundary.
use codexflow_domain::{AppError, ErrorCode};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

fn error(code: ErrorCode, message: &'static str, retryable: bool) -> AppError {
    AppError::text(code, message, retryable)
}

fn invalid_address() -> AppError {
    error(ErrorCode::TextInvalidAddress,
        "请填写 HTTPS 服务地址；本机测试可用回环 HTTP。可包含 /v1 或网关路径，不要包含端点、账号、查询或片段。", false)
}

pub fn normalize_text_base_url(input: &str) -> Result<String, AppError> {
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
        || parsed.path().contains('%')
        || parsed.path().contains('\\')
    {
        return Err(invalid_address());
    }
    let path = parsed.path().trim_end_matches('/').to_owned();
    if path.ends_with("/chat/completions")
        || path.ends_with("/completions")
        || path.ends_with("/models")
    {
        return Err(invalid_address());
    }
    let mut result = parsed;
    result.set_path(&path);
    Ok(result.as_str().trim_end_matches('/').to_owned())
}

fn endpoint(base_url: &str) -> Result<String, AppError> {
    let base = normalize_text_base_url(base_url)?;
    Ok(if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    })
}

#[derive(Clone, Debug)]
pub struct TextOutput {
    pub text: String,
    pub requested_model: String,
    pub actual_model: String,
}

pub struct TextCredential {
    pub base_url: String,
    pub key: String,
}

pub struct TextClient {
    client: Client,
}

impl TextClient {
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
                    ErrorCode::TextConnectionFailed,
                    "无法初始化文本服务连接。",
                    true,
                )
            })?;
        Ok(Self { client })
    }

    pub async fn complete(
        &self,
        credential: &TextCredential,
        model: &str,
        prompt: &str,
        cancel: CancellationToken,
    ) -> Result<TextOutput, AppError> {
        if credential.key.is_empty()
            || model.trim().is_empty()
            || model.chars().count() > 200
            || model.chars().any(char::is_control)
            || model.contains(&credential.key)
            || credential.base_url.contains(&credential.key)
        {
            return Err(error(
                ErrorCode::TextProtocolInvalid,
                "模型设置或请求材料无效；请检查输入。",
                false,
            ));
        }
        if prompt.contains(&credential.key) {
            return Err(error(
                ErrorCode::TextProtocolInvalid,
                "请求材料包含当前 API Key，已阻止发送；请轮换密钥或清理来源内容。",
                false,
            ));
        }
        let url = endpoint(&credential.base_url)?;
        let body = serde_json::json!({"model":model,"messages":[{"role":"user","content":prompt}]});
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            result = self.client.post(url).bearer_auth(&credential.key).json(&body).send() =>
                result.map_err(|err| if err.is_timeout() {
                    error(ErrorCode::AnalysisTimeout, "文本服务请求超时，请重试。", true)
                } else {
                    error(ErrorCode::TextConnectionFailed, "无法连接文本服务；请检查地址、网络和 TLS 证书。", true)
                })?,
        };
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        if response.content_length().is_some_and(|len| len > 1_000_000) {
            return Err(protocol_error());
        }
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            result = limited_bytes(response, 1_000_000) => result.ok_or_else(protocol_error)?,
        };
        let parsed: ChatResponse = serde_json::from_slice(&bytes).map_err(|_| protocol_error())?;
        let content = parsed
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_deref())
            .filter(|value| !value.trim().is_empty() && !value.contains(&credential.key))
            .ok_or_else(protocol_error)?;
        if parsed.model.trim().is_empty()
            || parsed.model.chars().count() > 200
            || parsed.model.chars().any(char::is_control)
            || parsed.model.contains(&credential.key)
        {
            return Err(protocol_error());
        }
        Ok(TextOutput {
            text: content.to_owned(),
            requested_model: model.to_owned(),
            actual_model: parsed.model,
        })
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    model: String,
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

fn cancelled() -> AppError {
    error(
        ErrorCode::AnalysisCancelled,
        "文本服务请求已取消；远端计算或计费可能继续。",
        false,
    )
}
fn protocol_error() -> AppError {
    error(
        ErrorCode::TextProtocolInvalid,
        "文本服务返回的数据不符合 Chat Completions 协议。",
        false,
    )
}
async fn limited_bytes(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().checked_add(chunk.len())? > limit {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}
async fn status_error(response: reqwest::Response) -> AppError {
    let status = response.status();
    // Only a known machine code is inspected. Provider prose may echo input or credentials.
    let code = if status == StatusCode::TOO_MANY_REQUESTS {
        limited_bytes(response, 4096)
            .await
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|body| {
                body.pointer("/error/code")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            })
    } else {
        None
    };
    if matches!(
        code.as_deref(),
        Some("insufficient_quota" | "quota_exceeded" | "billing_limit_exceeded")
    ) {
        return error(
            ErrorCode::AnalysisQuotaExceeded,
            "文本服务额度不足，请检查账户。",
            false,
        );
    }
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => error(
            ErrorCode::AnalysisAuthenticationFailed,
            "文本服务认证失败，请检查当前地址的 API Key。",
            false,
        ),
        StatusCode::PAYMENT_REQUIRED => error(
            ErrorCode::AnalysisQuotaExceeded,
            "文本服务额度不足，请检查账户。",
            false,
        ),
        StatusCode::TOO_MANY_REQUESTS => error(
            ErrorCode::AnalysisOverloaded,
            "文本服务限流，请稍后重试。",
            true,
        ),
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => {
            error(
                ErrorCode::AnalysisModelUnsupported,
                "文本服务拒绝请求；请检查模型 ID 和 Chat Completions 地址。",
                false,
            )
        }
        _ if status.is_server_error() => error(
            ErrorCode::AnalysisOverloaded,
            "文本服务暂不可用，请稍后重试。",
            true,
        ),
        _ => protocol_error(),
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

    fn server(status: u16, response: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                let header_end = request.windows(4).position(|part| part == b"\r\n\r\n");
                if let Some(end) = header_end {
                    let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(count > 0);
            }
            let reply = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len());
            socket.write_all(reply.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, handle)
    }
    #[test]
    fn url_accepts_version_and_gateway_prefix() {
        assert_eq!(
            endpoint("https://example.test/v1").unwrap(),
            "https://example.test/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://example.test/gateway").unwrap(),
            "https://example.test/gateway/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://example.test/models/proxy/v1").unwrap(),
            "https://example.test/models/proxy/v1/chat/completions"
        );
        assert!(normalize_text_base_url("https://user:secret@example.test/v1").is_err());
        assert!(normalize_text_base_url("https://example.test/v1?key=secret").is_err());
    }

    #[tokio::test]
    async fn request_uses_chat_protocol_without_secret_in_body() {
        let (address, handle) = server(
            200,
            r#"{"model":"resolved-model","choices":[{"message":{"content":"{\"name\":\"合成工作\"}"}}]}"#,
        );
        let credential = TextCredential {
            base_url: format!("{address}/gateway/v1"),
            key: "test-secret-key".into(),
        };
        let result = TextClient::new()
            .unwrap()
            .complete(
                &credential,
                "requested-model",
                "合成输入",
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.actual_model, "resolved-model");
        let request = handle.join().unwrap();
        assert!(request.starts_with("POST /gateway/v1/chat/completions HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-secret-key"));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        assert!(!body.contains("test-secret-key"));
        assert!(body.contains("requested-model"));
        assert!(body.contains("合成输入"));
    }

    #[tokio::test]
    async fn quota_and_protocol_failures_are_sanitized() {
        let (address, handle) = server(
            429,
            r#"{"error":{"code":"insufficient_quota","message":"test-secret-key"}}"#,
        );
        let credential = TextCredential {
            base_url: address,
            key: "test-secret-key".into(),
        };
        let error = TextClient::new()
            .unwrap()
            .complete(&credential, "model", "合成输入", CancellationToken::new())
            .await
            .unwrap_err();
        handle.join().unwrap();
        assert!(matches!(error.code, ErrorCode::AnalysisQuotaExceeded));
        assert!(!error.message.contains("test-secret-key"));
        let secret_in_prompt = TextClient::new()
            .unwrap()
            .complete(
                &credential,
                "model",
                "test-secret-key",
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            secret_in_prompt.code,
            ErrorCode::TextProtocolInvalid
        ));
    }
}
