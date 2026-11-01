use crate::assistant::providers::{anthropic, cli, openai, openrouter};
use crate::assistant::types::{ProviderConnection, ProviderDescriptor};

use super::anthropic::AnthropicAdapter;
use super::cli::CliAdapter;
use super::openai::OpenAiAdapter;
use super::types::{ProviderAdapter, ProviderError};

pub fn supported_providers() -> Vec<ProviderDescriptor> {
    let mut providers = vec![
        openai::provider_descriptor(),
        anthropic::provider_descriptor(),
    ];
    providers.extend(cli::provider_descriptors());
    providers
}

pub fn get_provider_descriptor(provider_id: &str) -> Option<ProviderDescriptor> {
    supported_providers()
        .into_iter()
        .find(|provider| provider.id == provider_id)
}

pub fn resolve_adapter(provider_id: &str) -> Result<Box<dyn ProviderAdapter>, ProviderError> {
    match provider_id {
        openai::OPENAI_PROVIDER_ID => Ok(Box::new(OpenAiAdapter)),
        anthropic::ANTHROPIC_PROVIDER_ID => Ok(Box::new(AnthropicAdapter)),
        cli::CLAUDE_CODE_PROVIDER_ID | cli::CODEX_PROVIDER_ID | cli::OPENCODE_PROVIDER_ID => {
            CliAdapter::new(provider_id)
                .map(|adapter| Box::new(adapter) as Box<dyn ProviderAdapter>)
                .ok_or(ProviderError::NotConfigured)
        }
        _ => Err(ProviderError::NotConfigured),
    }
}

pub fn is_cli_provider(provider_id: &str) -> bool {
    cli::is_cli_provider(provider_id)
}

/// Resolve brand-specific behavior at the same boundary as protocol behavior.
/// Unknown and custom brands retain their protocol adapter.
pub fn resolve_connection_adapter(
    connection: &ProviderConnection,
) -> Result<Box<dyn ProviderAdapter>, ProviderError> {
    if connection.protocol_id == openai::OPENAI_PROVIDER_ID
        && connection.provider_id == openrouter::OPENROUTER_PROVIDER_ID
    {
        Ok(Box::new(openrouter::OpenRouterAdapter))
    } else {
        resolve_adapter(&connection.protocol_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::AuthMode;

    #[test]
    fn brand_adapter_selection_preserves_protocol_fallback() {
        let mut connection = ProviderConnection {
            id: "test".into(),
            name: "test".into(),
            protocol_id: openai::OPENAI_PROVIDER_ID.into(),
            provider_id: openrouter::OPENROUTER_PROVIDER_ID.into(),
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: "test".into(),
            model_id: "test".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        assert_eq!(
            resolve_connection_adapter(&connection)
                .unwrap()
                .provider_id(),
            openrouter::OPENROUTER_PROVIDER_ID
        );
        connection.provider_id = "custom".into();
        assert_eq!(
            resolve_connection_adapter(&connection)
                .unwrap()
                .provider_id(),
            openai::OPENAI_PROVIDER_ID
        );
        connection.protocol_id = anthropic::ANTHROPIC_PROVIDER_ID.into();
        connection.provider_id = openrouter::OPENROUTER_PROVIDER_ID.into();
        assert_eq!(
            resolve_connection_adapter(&connection)
                .unwrap()
                .provider_id(),
            anthropic::ANTHROPIC_PROVIDER_ID
        );
    }
}
