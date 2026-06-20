pub mod anthropic;
pub mod cli;
pub mod openai;
pub mod registry;
pub mod types;

pub use registry::{
    get_provider_descriptor, is_cli_provider, resolve_adapter, supported_providers,
};

/// Whether a connection's active model can accept image input.
///
/// The per-model `supports_images` flag is just storage; this is the *gate*.
/// A bare per-id lookup would miss the commonest CLI case: a connection on the
/// **default model** stores `model_id = ""`, which matches no concrete id in
/// the static lists. So resolution is two-tier:
///
/// 1. CLI providers (static model lists): exact id match → that model's flag;
///    else (default / unknown id) → the provider-level answer
///    (`all(models, supports_images)`).
/// 2. API providers (dynamic `/v1/models` lists, user-set base_url → the active
///    model can be anything): a best-effort per-provider constant.
///
/// Single source of truth for both the backend history send-filter and the FE
/// paste/attach gate, so the two cannot drift.
pub fn connection_supports_images(provider_id: &str, model_id: &str) -> bool {
    if let Some(models) = cli::models_for_provider(provider_id) {
        let trimmed = model_id.trim();
        if !trimmed.is_empty() {
            if let Some(model) = models.iter().find(|m| m.id == trimmed) {
                return model.supports_images;
            }
        }
        // Default / unknown model id → provider-level answer.
        return models.iter().all(|m| m.supports_images);
    }
    // API providers: best-effort per-provider constant (mirrors the blanket
    // value their adapters stamp onto every dynamically-fetched model). The
    // common Anthropic and OpenAI chat models are vision-capable.
    matches!(
        provider_id,
        anthropic::ANTHROPIC_PROVIDER_ID | openai::OPENAI_PROVIDER_ID
    )
}

/// Parse a tool call's accumulated raw `arguments` text into params.
///
/// Empty text parses to `{}` — a tool legitimately called with no
/// arguments. Non-empty text that is not valid JSON is preserved as
/// `{"invalid_json": "<raw>"}` instead of being silently degraded to
/// `{}`: the object shape survives the round-trip back into provider
/// history, the schema gate still rejects it (additionalProperties:
/// false names the key), and the UI/DB record exactly what the model
/// emitted instead of an empty object it never sent.
pub(crate) fn parse_tool_arguments(tool_name: &str, raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                "tool `{tool_name}`: arguments are not valid JSON ({error}); preserving raw text"
            );
            serde_json::json!({ "invalid_json": raw })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cli::{CLAUDE_CODE_PROVIDER_ID, CODEX_PROVIDER_ID, OPENCODE_PROVIDER_ID};
    use super::connection_supports_images;
    use super::parse_tool_arguments;

    #[test]
    fn image_gate_resolves_default_and_explicit_models() {
        // Default model (empty id) is the commonest CLI case: must resolve via
        // the provider-level answer, not a (failing) per-id lookup.
        assert!(connection_supports_images(CLAUDE_CODE_PROVIDER_ID, ""));
        assert!(connection_supports_images(CODEX_PROVIDER_ID, "  "));
        assert!(!connection_supports_images(OPENCODE_PROVIDER_ID, ""));

        // Explicit known id uses that model's flag.
        assert!(connection_supports_images(
            CLAUDE_CODE_PROVIDER_ID,
            "sonnet"
        ));
        // Unknown id for a uniformly-capable provider falls back to the
        // provider answer (still true), not false.
        assert!(connection_supports_images(
            CODEX_PROVIDER_ID,
            "gpt-9-future"
        ));

        // API providers: best-effort per-provider constant. Anthropic and
        // OpenAI chat models are vision-capable; their adapters send images.
        assert!(connection_supports_images("anthropic", ""));
        assert!(connection_supports_images("openai", ""));
        // Unknown provider → conservative false.
        assert!(!connection_supports_images("acme", ""));
    }

    #[test]
    fn empty_arguments_parse_to_empty_object() {
        assert_eq!(parse_tool_arguments("t", ""), serde_json::json!({}));
        assert_eq!(parse_tool_arguments("t", "  "), serde_json::json!({}));
    }

    #[test]
    fn valid_json_passes_through() {
        assert_eq!(
            parse_tool_arguments("t", r#"{"command":"ls"}"#),
            serde_json::json!({"command": "ls"})
        );
    }

    #[test]
    fn malformed_json_is_preserved_not_dropped() {
        let raw = r#"{"command": "ls", "cwd": oops}"#;
        assert_eq!(
            parse_tool_arguments("t", raw),
            serde_json::json!({"invalid_json": raw})
        );
    }
}
