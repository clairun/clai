pub mod anthropic;
pub mod cli;
pub mod openai;
pub mod registry;
pub mod types;

pub use registry::{
    get_provider_descriptor, is_cli_provider, resolve_adapter, supported_providers,
};

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
    use super::parse_tool_arguments;

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
