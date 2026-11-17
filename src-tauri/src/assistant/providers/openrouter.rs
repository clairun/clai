//! OpenRouter behavior layered over the shared OpenAI-compatible transport.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;

use crate::assistant::types::{
    CompletionRequest, ModelInfo, ProtocolFamily, ProviderConnection, ProviderEvent,
};

use super::failure::read_failure;
use super::openai::{endpoint_url, get_api_key, OpenAiAdapter};
use super::types::{ProviderAdapter, ProviderError};

pub const OPENROUTER_PROVIDER_ID: &str = "openrouter";

pub struct OpenRouterAdapter;

#[async_trait]
impl ProviderAdapter for OpenRouterAdapter {
    fn provider_id(&self) -> &'static str {
        OPENROUTER_PROVIDER_ID
    }

    fn protocol_family(&self) -> ProtocolFamily {
        ProtocolFamily::OpenAiCompatible
    }

    async fn validate_connection(
        &self,
        connection: &ProviderConnection,
    ) -> Result<(), ProviderError> {
        validate_openrouter_key(connection).await
    }

    async fn list_models(
        &self,
        connection: &ProviderConnection,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        OpenAiAdapter.list_models(connection).await
    }

    async fn stream_completion(
        &self,
        connection: &ProviderConnection,
        request: CompletionRequest,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        OpenAiAdapter.stream_completion(connection, request).await
    }
}

async fn validate_openrouter_key(connection: &ProviderConnection) -> Result<(), ProviderError> {
    let key = get_api_key(connection)?.ok_or(ProviderError::NotConfigured)?;
    validate_openrouter_key_at(&key_validation_url(connection), &key).await
}

/// OpenRouter's authenticated key endpoint, under the connection's API root
/// like every other URL in this adapter, so a custom base validates against
/// itself instead of openrouter.ai proper.
fn key_validation_url(connection: &ProviderConnection) -> String {
    endpoint_url(connection, "key")
}

async fn validate_openrouter_key_at(url: &str, key: &str) -> Result<(), ProviderError> {
    let response = Client::new()
        .get(url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|error| ProviderError::RequestFailed(error.to_string()))?;
    if !response.status().is_success() {
        return Err(ProviderError::Failed(
            read_failure(OPENROUTER_PROVIDER_ID, response).await,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::providers::failure::FailureKind;

    fn connection() -> ProviderConnection {
        ProviderConnection {
            id: "test".into(),
            name: "OpenRouter".into(),
            protocol_id: "openai".into(),
            provider_id: "openrouter".into(),
            auth_mode: crate::assistant::types::AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: "provider-connection::test".into(),
            model_id: "openrouter/auto".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn key_validation_url_follows_the_connection_base() {
        let default_url = key_validation_url(&connection());
        assert_eq!(default_url, "https://openrouter.ai/api/v1/key");

        let mut proxied = connection();
        proxied.base_url = Some("https://proxy.example/openrouter/api/v1/".into());
        assert_eq!(
            key_validation_url(&proxied),
            "https://proxy.example/openrouter/api/v1/key"
        );

        let mut bare = connection();
        bare.base_url = Some("http://127.0.0.1:1".into());
        assert_eq!(key_validation_url(&bare), "http://127.0.0.1:1/v1/key");
    }

    #[tokio::test]
    async fn openrouter_key_validation_rejects_unauthorized_response() {
        use axum::{http::HeaderMap, routing::get, Router};

        let router = Router::new().route(
            "/key",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer invalid-key");
                axum::http::StatusCode::UNAUTHORIZED
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/key", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        assert!(matches!(
            validate_openrouter_key_at(&url, "invalid-key").await,
            Err(ProviderError::Failed(failure))
                if failure.status == Some(401) && failure.kind == FailureKind::Auth
        ));
    }
}
