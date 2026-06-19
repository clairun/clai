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
                supports_images: true,
            },
            ModelInfo {
                id: "opus".to_string(),
                display_name: "Opus".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "haiku".to_string(),
                display_name: "Haiku".to_string(),
                supports_tools: true,
                supports_images: true,
            },
        ],
        CODEX_PROVIDER_ID => vec![
            ModelInfo {
                id: "gpt-5.5".to_string(),
                display_name: "GPT-5.5".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.4".to_string(),
                display_name: "GPT-5.4".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.4-mini".to_string(),
                display_name: "GPT-5.4 Mini".to_string(),
                supports_tools: true,
                supports_images: true,
            },
            ModelInfo {
                id: "gpt-5.3-codex".to_string(),
                display_name: "GPT-5.3 Codex".to_string(),
                supports_tools: true,
                supports_images: true,
            },
        ],
        OPENCODE_PROVIDER_ID => vec![ModelInfo {
            id: "default".to_string(),
            display_name: "Default".to_string(),
            supports_tools: true,
            // OpenCode fronts arbitrary models via Models.dev; the "default"
            // id can't tell us if the active model is vision-capable, so gate
            // images OFF until per-model capability is resolvable.
            supports_images: false,
        }],
        _ => return None,
    };
    Some(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_capability_gated_per_provider() {
        // Codex CLI ingests images via `codex exec --image <FILE>`; Claude Code
        // accepts image content blocks — both report vision-capable.
        for provider in [CODEX_PROVIDER_ID, CLAUDE_CODE_PROVIDER_ID] {
            let models = models_for_provider(provider).unwrap();
            assert!(!models.is_empty());
            assert!(
                models.iter().all(|m| m.supports_images),
                "{provider} models should support images"
            );
        }
        // OpenCode fronts arbitrary models; gated off until per-model vision
        // capability is resolvable.
        let opencode = models_for_provider(OPENCODE_PROVIDER_ID).unwrap();
        assert!(opencode.iter().all(|m| !m.supports_images));
    }
}
