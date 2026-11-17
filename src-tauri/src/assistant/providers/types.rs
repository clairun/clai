#![allow(dead_code)]

use std::path::Path;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use thiserror::Error;

use super::failure::{FailureKind, ProviderFailure};
use crate::assistant::types::{
    CompletionRequest, ModelInfo, ProtocolFamily, ProviderConnection, ProviderEvent,
};

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>;

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
    /// Failed before or without an HTTP status: transport, decoding, CLI.
    #[error("provider request failed: {0}")]
    RequestFailed(String),
    /// The provider answered with a non-success HTTP status.
    #[error("provider request failed: {0}")]
    Failed(ProviderFailure),
}

impl ProviderError {
    pub fn kind(&self) -> Option<FailureKind> {
        match self {
            Self::Failed(failure) => Some(failure.kind),
            _ => None,
        }
    }

    /// Whether compacting the conversation may let the request through. A
    /// classified failure says so by its kind; anything else by its text.
    pub fn is_context_limit(&self) -> bool {
        match self {
            Self::Failed(failure) => failure.kind == FailureKind::ContextTooLong,
            other => is_context_limit_error(&other.to_string()),
        }
    }
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;
    fn protocol_family(&self) -> ProtocolFamily;

    async fn validate_connection(
        &self,
        _connection: &ProviderConnection,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

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
    use super::*;
    use crate::assistant::providers::failure::classify_http;

    #[test]
    fn context_limit_classifier_matches_codex_turn_start_input_too_large() {
        assert!(is_context_limit_error(
            "Error: turn/start: Input exceeds the maximum length of 1048576 characters (input_too_large, actual_chars=1072355)"
        ));
    }

    #[test]
    fn a_classified_failure_is_a_context_limit_by_kind_only() {
        let throttled = classify_http(
            "anthropic",
            429,
            None,
            "rate limit of 30,000 input tokens per minute",
        );
        assert!(!ProviderError::Failed(throttled).is_context_limit());
        let too_long = classify_http(
            "zai",
            400,
            None,
            r#"{"error":{"code":"1261","message":"Prompt exceeds max length"}}"#,
        );
        assert!(ProviderError::Failed(too_long).is_context_limit());
        assert!(ProviderError::RequestFailed("prompt is too long".into()).is_context_limit());
    }
}
