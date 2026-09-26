use super::SourceService;
use codexflow_domain::{
    AppError, EmbeddingConfig, EmbeddingStatus, EmbeddingValidation, ErrorCode,
};
use codexflow_embedding::{normalize_embedding_base_url, EmbeddingClient, EmbeddingCredential};
use codexflow_jev::Credential;
use std::time::Duration;
use tokio::sync::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

fn credential_error() -> AppError {
    AppError::embedding(
        ErrorCode::EmbeddingCredentialFailed,
        "无法访问系统凭据库。请检查凭据库状态后重试。",
        true,
    )
}

fn not_configured() -> AppError {
    AppError::embedding(
        ErrorCode::EmbeddingNotConfigured,
        "请先保存嵌入服务地址、模型及当前地址的 API Key。",
        false,
    )
}

impl SourceService {
    pub async fn embedding_status(&self) -> EmbeddingStatus {
        let config = self.preferences.lock().await.embedding.clone();
        let (credential_configured, credential_error) = match self.embedding_credentials.load() {
            Ok(value) => (
                value.is_some_and(|item| item.base_url == config.base_url),
                None,
            ),
            Err(_) => (false, Some(credential_error())),
        };
        EmbeddingStatus {
            config,
            credential_configured,
            credential_error,
        }
    }

    pub async fn save_embedding(
        &self,
        base_url: String,
        model: String,
        api_key: Option<String>,
    ) -> Result<EmbeddingStatus, AppError> {
        let base_url = normalize_embedding_base_url(&base_url)?;
        let model = model.trim().to_owned();
        if model.is_empty() || model.chars().count() > 200 || model.chars().any(char::is_control) {
            return Err(AppError::embedding(
                ErrorCode::EmbeddingModelUnsupported,
                "请填写有效的嵌入模型 ID。",
                false,
            ));
        }
        if api_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.chars().any(char::is_control))
        {
            return Err(not_configured());
        }
        let _gate = self.stop_embedding().await;
        let mut preferences = self.preferences.lock().await;
        if preferences.embedding.base_url != base_url && api_key.is_none() {
            return Err(AppError::embedding(
                ErrorCode::EmbeddingNotConfigured,
                "更换嵌入服务地址时请填写新 API Key；旧密钥不会发送到新地址。",
                false,
            ));
        }
        let old_credential = self
            .embedding_credentials
            .load()
            .map_err(|_| credential_error())?;
        let contains_key = api_key
            .as_deref()
            .map(str::trim)
            .into_iter()
            .chain(old_credential.as_ref().map(|item| item.key.as_str()))
            .any(|key| base_url.contains(key) || model.contains(key));
        if contains_key {
            return Err(AppError::embedding(
                ErrorCode::EmbeddingInvalidAddress,
                "服务地址和模型 ID 不能包含 API Key。",
                false,
            ));
        }
        let replacing = api_key.is_some();
        if let Some(key) = api_key {
            self.embedding_credentials
                .save(&Credential {
                    base_url: base_url.clone(),
                    key: key.trim().into(),
                })
                .map_err(|_| credential_error())?;
        }
        let mut next = preferences.clone();
        let config = EmbeddingConfig { base_url, model };
        if next.embedding != config {
            next.embedding_revision = next.embedding_revision.saturating_add(1);
        }
        next.embedding = config;
        if let Err(error) = self.store.save(&next) {
            if replacing {
                let restored = if let Some(previous) = old_credential {
                    self.embedding_credentials.save(&previous)
                } else {
                    self.embedding_credentials.delete()
                };
                if restored.is_err() {
                    return Err(credential_error());
                }
            }
            return Err(error);
        }
        *preferences = next;
        drop(preferences);
        Ok(self.embedding_status().await)
    }

    pub async fn delete_embedding_credential(&self) -> Result<EmbeddingStatus, AppError> {
        let _gate = self.stop_embedding().await;
        self.embedding_credentials
            .delete()
            .map_err(|_| credential_error())?;
        Ok(self.embedding_status().await)
    }

    async fn stop_embedding(&self) -> RwLockWriteGuard<'_, ()> {
        self.embedding_cancel.lock().await.cancel();
        self.embedding_validation_cancel.lock().await.cancel();
        let gate = self.embedding_gate.write().await;
        *self.embedding_cancel.lock().await = CancellationToken::new();
        *self.embedding_validation_cancel.lock().await = CancellationToken::new();
        gate
    }

    pub async fn cancel_embedding_request(&self) {
        let mut token = self.embedding_validation_cancel.lock().await;
        token.cancel();
        *token = CancellationToken::new();
    }

    pub async fn test_embedding(&self) -> Result<EmbeddingValidation, AppError> {
        let mut token = self.embedding_validation_cancel.lock().await;
        token.cancel();
        *token = CancellationToken::new();
        let cancel = token.clone();
        drop(token);
        let config = self.preferences.lock().await.embedding.clone();
        let output = self
            .embedding_complete(
                &config,
                None,
                &["CodexFlow 固定合成文本向量连通性检查。".into()],
                cancel,
            )
            .await?;
        Ok(EmbeddingValidation {
            requested_model: output.requested_model,
            actual_model: output.actual_model,
            vector_dimensions: output.vectors.first().map_or(0, Vec::len),
            input_tokens: output.input_tokens,
        })
    }

    pub(crate) async fn embedding_config(&self) -> EmbeddingConfig {
        self.preferences.lock().await.embedding.clone()
    }

    pub(crate) async fn embedding_complete(
        &self,
        expected: &EmbeddingConfig,
        expected_revision: Option<u64>,
        inputs: &[String],
        cancel: CancellationToken,
    ) -> Result<codexflow_embedding::EmbeddingOutput, AppError> {
        let _gate = self.embedding_gate.read().await;
        let settings_cancel = self.embedding_cancel.lock().await.clone();
        let preferences = self.preferences.lock().await.clone();
        let config = preferences.embedding;
        if config != *expected
            || expected_revision.is_some_and(|revision| revision != preferences.embedding_revision)
        {
            return Err(AppError::embedding(
                ErrorCode::AnalysisConfigChanged,
                "嵌入服务设置已变化，旧请求结果不会保存。",
                false,
            ));
        }
        if config.base_url.is_empty() || config.model.is_empty() {
            return Err(not_configured());
        }
        let credential = self
            .embedding_credentials
            .load()
            .map_err(|_| credential_error())?
            .filter(|item| item.base_url == config.base_url)
            .ok_or_else(not_configured)?;
        let client = EmbeddingClient::with_timeout(Duration::from_secs(180))?;
        let embedding_credential = EmbeddingCredential {
            base_url: credential.base_url,
            key: credential.key,
        };
        let request_cancel = cancel.clone();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AppError::embedding(
                ErrorCode::EmbeddingCancelled,
                "嵌入请求已取消；远端计算或计费可能继续。",
                false,
            )),
            _ = settings_cancel.cancelled() => Err(AppError::embedding(
                ErrorCode::AnalysisConfigChanged,
                "嵌入服务设置已变化，旧请求结果不会保存。",
                false,
            )),
            result = client.embed(
                &embedding_credential,
                &config.model,
                inputs,
                request_cancel,
            ) => result,
        }
    }
}
