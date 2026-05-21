use crate::assistant::types::{AuthMode, ModelInfo, ProtocolFamily, ProviderDescriptor};

pub const CLAUDE_CODE_PROVIDER_ID: &str = "claude-code";
pub const CODEX_PROVIDER_ID: &str = "codex";
pub const OPENCODE_PROVIDER_ID: &str = "opencode";

pub fn provider_descriptors() -> Vec<ProviderDescriptor> {
    vec![
        ProviderDescriptor {
            id: CLAUDE_CODE_PROVIDER_ID.to_string(),
            display_name: "Claude Code".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
        ProviderDescriptor {
            id: CODEX_PROVIDER_ID.to_string(),
            display_name: "Codex CLI".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
        ProviderDescriptor {
            id: OPENCODE_PROVIDER_ID.to_string(),
            display_name: "OpenCode".to_string(),
            protocol_family: ProtocolFamily::Custom,
            supported_auth_modes: vec![AuthMode::SubscriptionLogin],
            configurable_base_url: true,
            is_cli_backed: true,
        },
    ]
}

pub fn is_cli_provider(provider_id: &str) -> bool {
    matches!(
        provider_id,
        CLAUDE_CODE_PROVIDER_ID | CODEX_PROVIDER_ID | OPENCODE_PROVIDER_ID
    )
}

pub fn command_for_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        CLAUDE_CODE_PROVIDER_ID => Some("claude"),
        CODEX_PROVIDER_ID => Some("codex"),
        OPENCODE_PROVIDER_ID => Some("opencode"),
        _ => None,
    }
}

pub fn models_for_provider(provider_id: &str) -> Option<Vec<ModelInfo>> {
    let models = match provider_id {
        CLAUDE_CODE_PROVIDER_ID => vec![
            ModelInfo {
                id: "sonnet".to_string(),
                display_name: "Sonnet".to_string(),
                supports_tools: true,
            },
            ModelInfo {
                id: "opus".to_string(),
                display_name: "Opus".to_string(),
                supports_tools: true,
            },
            ModelInfo {
                id: "haiku".to_string(),
                display_name: "Haiku".to_string(),
                supports_tools: true,
            },
        ],
        CODEX_PROVIDER_ID => vec![ModelInfo {
            id: "default".to_string(),
            display_name: "Default".to_string(),
            supports_tools: true,
        }],
        OPENCODE_PROVIDER_ID => vec![ModelInfo {
            id: "default".to_string(),
            display_name: "Default".to_string(),
            supports_tools: true,
        }],
        _ => return None,
    };
    Some(models)
}
