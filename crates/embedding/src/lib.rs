//! OpenAI-compatible embedding requests. Only validated vectors cross this adapter.
use codexflow_domain::{AppError, ErrorCode};
use reqwest::{redirect::Policy, Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const RESPONSE_LIMIT: usize = 16_000_000;
const MAX_INPUTS: usize = 32;
const MAX_INPUT_CHARS: usize = 32_000;
const MAX_VECTOR_DIMENSIONS: usize = 16_384;

#[derive(Clone, Debug)]
pub struct EmbeddingCredential {
    pub base_url: String,
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct EmbeddingOutput {
    pub vectors: Vec<Vec<f32>>,
    pub requested_model: String,
    pub actual_model: String,
    pub input_tokens: u64,
}

pub struct EmbeddingClient {
    client: Client,
}

fn error(code: ErrorCode, message: &'static str, retryable: bool) -> AppError {
    AppError::embedding(code, message, retryable)
}

pub fn normalize_embedding_base_url(input: &str) -> Result<String, AppError> {
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
    if path.ends_with("/v1/embeddings")
        || path.ends_with("/embeddings")
        || path.ends_with("/models")
    {
        return Err(invalid_address());
    }
    let mut result = parsed;
    result.set_path(&path);
    Ok(result.as_str().trim_end_matches('/').to_owned())
}

fn endpoint(base_url: &str) -> Result<String, AppError> {
    let base = normalize_embedding_base_url(base_url)?;
    Ok(if base.ends_with("/v1") {
        format!("{base}/embeddings")
    } else {
        format!("{base}/v1/embeddings")
    })
}

impl EmbeddingClient {
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
                    ErrorCode::EmbeddingConnectionFailed,
                    "无法初始化嵌入服务连接。",
                    true,
                )
            })?;
        Ok(Self { client })
    }

    pub async fn embed(
        &self,
        credential: &EmbeddingCredential,
        model: &str,
        inputs: &[String],
        cancel: CancellationToken,
    ) -> Result<EmbeddingOutput, AppError> {
        if credential.key.trim().is_empty()
            || model.trim().is_empty()
            || model.chars().count() > 200
            || model.chars().any(char::is_control)
            || model.contains(&credential.key)
            || credential.base_url.contains(&credential.key)
            || inputs.is_empty()
            || inputs.len() > MAX_INPUTS
            || inputs.iter().any(|input| {
                input.trim().is_empty()
                    || input.chars().count() > MAX_INPUT_CHARS
                    || input.contains(&credential.key)
            })
        {
            return Err(error(
                ErrorCode::EmbeddingProtocolInvalid,
                "嵌入模型设置或请求材料无效；请检查输入。",
                false,
            ));
        }
        let url = endpoint(&credential.base_url)?;
        let body = serde_json::json!({
            "model": model,
            "input": inputs,
            "encoding_format": "float"
        });
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(error(
                ErrorCode::EmbeddingCancelled,
                "嵌入请求已取消；远端计算或计费可能继续。",
                false,
            )),
            result = self.client.post(url).bearer_auth(&credential.key).json(&body).send() =>
                result.map_err(|err| if err.is_timeout() {
                    error(ErrorCode::EmbeddingTimeout, "嵌入服务请求超时，请重试。", true)
                } else {
                    error(ErrorCode::EmbeddingConnectionFailed, "无法连接嵌入服务；请检查地址、网络和 TLS 证书。", true)
                })?,
        };
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        if response
            .content_length()
            .is_some_and(|len| len as usize > RESPONSE_LIMIT)
        {
            return Err(protocol_error());
        }
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(error(
                ErrorCode::EmbeddingCancelled,
                "嵌入请求已取消；远端计算或计费可能继续。",
                false,
            )),
            result = limited_bytes(response, RESPONSE_LIMIT) => result.ok_or_else(protocol_error)?,
        };
        let parsed: EmbeddingsResponse =
            serde_json::from_slice(&bytes).map_err(|_| protocol_error())?;
        if parsed.model.trim().is_empty()
            || parsed.model.chars().count() > 200
            || parsed.model.chars().any(char::is_control)
            || parsed.model.contains(&credential.key)
            || parsed.data.len() != inputs.len()
        {
            return Err(protocol_error());
        }
        let mut ordered: Vec<Option<Vec<f32>>> = vec![None; inputs.len()];
        let mut dimensions = None;
        for item in parsed.data {
            if item.index >= ordered.len()
                || ordered[item.index].is_some()
                || item.embedding.is_empty()
                || item.embedding.len() > MAX_VECTOR_DIMENSIONS
                || dimensions.is_some_and(|value| value != item.embedding.len())
                || item.embedding.iter().any(|value| !value.is_finite())
            {
                return Err(protocol_error());
            }
            dimensions = Some(item.embedding.len());
            ordered[item.index] = Some(item.embedding);
        }
        let vectors = ordered
            .into_iter()
            .map(|item| item.ok_or_else(protocol_error))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EmbeddingOutput {
            vectors,
            requested_model: model.to_owned(),
            actual_model: parsed.model,
            input_tokens: parsed.usage.total_tokens,
        })
    }
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
    model: String,
    #[serde(default)]
    usage: EmbeddingUsage,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Default, Deserialize)]
struct EmbeddingUsage {
    total_tokens: u64,
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
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => error(
            ErrorCode::EmbeddingAuthenticationFailed,
            "嵌入服务认证失败，请检查当前地址的 API Key。",
            false,
        ),
        StatusCode::PAYMENT_REQUIRED => error(
            ErrorCode::EmbeddingQuotaExceeded,
            "嵌入服务额度不足，请检查账户。",
            false,
        ),
        StatusCode::TOO_MANY_REQUESTS => error(
            ErrorCode::EmbeddingOverloaded,
            "嵌入服务限流，请稍后重试。",
            true,
        ),
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => {
            error(
                ErrorCode::EmbeddingModelUnsupported,
                "嵌入服务拒绝请求；请检查模型 ID 和 Embeddings 地址。",
                false,
            )
        }
        _ if status.is_server_error() => error(
            ErrorCode::EmbeddingOverloaded,
            "嵌入服务暂不可用，请稍后重试。",
            true,
        ),
        _ => protocol_error(),
    }
}

fn invalid_address() -> AppError {
    error(
        ErrorCode::EmbeddingInvalidAddress,
        "请填写 HTTPS 嵌入服务根地址；本机测试可使用回环 HTTP。",
        false,
    )
}

fn protocol_error() -> AppError {
    error(
        ErrorCode::EmbeddingProtocolInvalid,
        "嵌入服务返回的数据不符合 OpenAI Embeddings 协议。",
        false,
    )
}
