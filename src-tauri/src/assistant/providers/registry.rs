use crate::assistant::providers::{anthropic, cli, openai};
use crate::assistant::types::ProviderDescriptor;

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
