#![allow(dead_code)]

use std::path::Path;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use thiserror::Error;

use crate::assistant::types::{
    CompletionRequest, ModelInfo, ProtocolFamily, ProviderConnection, ProviderEvent,
};

pub fn is_context_limit_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "context length",
        "context window",
        "maximum context",
        "max context",
        "too many tokens",
        "token limit",
        "prompt is too long",
        "prompt too long",
        "input is too long",
        "input exceeds the maximum length",
        "input_too_large",
        "input tokens",
        "exceeds the model",
        "exceeds context",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider is not configured")]
    NotConfigured,
    #[error("provider transport is not implemented yet")]
    NotImplemented,
    #[error("provider request failed: {0}")]
    RequestFailed(String),
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;
    fn protocol_family(&self) -> ProtocolFamily;

    async fn list_models(
        &self,
        _connection: &ProviderConnection,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        Err(ProviderError::NotImplemented)
    }

    async fn stream_completion(
        &self,
        _connection: &ProviderConnection,
        _request: CompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        Err(ProviderError::NotImplemented)
    }

    async fn stream_sessionless_completion(
        &self,
        connection: &ProviderConnection,
        request: CompletionRequest,
        _working_dir: Option<&Path>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        self.stream_completion(connection, request).await
    }

    async fn cancel(&self, _provider_run_id: &str) -> Result<(), ProviderError> {
        Err(ProviderError::NotImplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::is_context_limit_error;

    #[test]
    fn context_limit_classifier_matches_codex_turn_start_input_too_large() {
        assert!(is_context_limit_error(
            "Error: turn/start: Input exceeds the maximum length of 1048576 characters (input_too_large, actual_chars=1072355)"
        ));
    }
}
