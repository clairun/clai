pub mod anthropic;
pub mod catalog;
pub mod cli;
pub mod failure;
pub mod openai;
pub mod openrouter;
pub mod registry;
pub mod retry;
pub mod types;
pub mod wire_log;

use crate::assistant::types::{ModelInfo, ProviderConnection};

pub use registry::{
    get_provider_descriptor, is_cli_provider, resolve_connection_adapter, supported_providers,
};

/// Whether a connection's active model can accept image input.
///
/// The per-model `supports_images` flag is just storage; this is the *gate*.
/// A bare per-id lookup would miss the commonest CLI case: a connection on the
/// **default model** stores `model_id = ""`, which matches no concrete id in
/// the static lists. So resolution is three-tier:
///
/// 1. CLI providers (static model lists): exact id match → that model's flag;
///    else (default / unknown id) → the provider-level answer
///    (`all(models, supports_images)`).
/// 2. Catalog entries may override the provider-level default when protocol and
///    brand capabilities differ (e.g. MiniMax uses Anthropic but is non-vision).
/// 3. API providers (dynamic `/v1/models` lists, user-set base_url → the active
///    model can be anything): a best-effort per-protocol constant.
///
/// Single source of truth for both the backend history send-filter and the FE
/// paste/attach gate, so the two cannot drift.
pub fn connection_supports_images(connection: &ProviderConnection) -> bool {
    if let Some(entry) = catalog::get_entry(&connection.provider_id) {
        if let Some(caps) = entry.capabilities {
            return caps.supports_images;
        }
    }

    if let Some(models) = cli::models_for_provider(&connection.protocol_id) {
        let trimmed = connection.model_id.trim();
        if !trimmed.is_empty() {
            if let Some(model) = models.iter().find(|m| m.id == trimmed) {
                return model.supports_images;
            }
        }
        // Default / unknown model id → provider-level answer.
        return models.iter().all(|m| m.supports_images);
    }
    // API providers: best-effort per-protocol constant (mirrors the blanket
    // value their adapters stamp onto every dynamically-fetched model). The
    // common Anthropic and OpenAI chat models are vision-capable.
    matches!(
        connection.protocol_id.as_str(),
        anthropic::ANTHROPIC_PROVIDER_ID | openai::OPENAI_PROVIDER_ID
    )
}

/// Where a connection's models are listed, per its catalog entry
/// (`catalog::entry_for_connection`).
enum ModelsSource {
    /// The entry's curated list; nothing is fetched.
    Curated(Vec<ModelInfo>),
    /// A dedicated OpenAI-style models URL apart from the chat base.
    OpenAiCompatible {
        url: String,
        capabilities: Option<catalog::ProviderCaps>,
    },
    /// The adapter's own `models` endpoint.
    Endpoint(String),
}

/// Routes model listing for both adapters; `endpoint_url` is the adapter's
/// URL rule.
fn models_source(
    connection: &ProviderConnection,
    endpoint_url: fn(&ProviderConnection, &str) -> String,
) -> ModelsSource {
    let Some(entry) = catalog::entry_for_connection(connection) else {
        return ModelsSource::Endpoint(endpoint_url(connection, "models"));
    };
    match entry.models_endpoint_style {
        catalog::ModelsEndpointStyle::None => ModelsSource::Curated(entry.curated_models),
        catalog::ModelsEndpointStyle::OpenAiCompatible { url } => ModelsSource::OpenAiCompatible {
            url,
            capabilities: entry.capabilities,
        },
        catalog::ModelsEndpointStyle::Standard => {
            ModelsSource::Endpoint(endpoint_url(connection, "models"))
        }
    }
}

/// Perform the wire-logged GET and return the response text. Non-2xx bodies
/// and transport failures are logged; a non-2xx is classified for `brand`.
pub(crate) async fn get_logged(
    wire: &wire_log::Scope,
    brand: &str,
    url: &str,
    headers: &[(String, String)],
) -> Result<String, types::ProviderError> {
    let mut builder = reqwest::Client::new().get(url);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let resp = match builder.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(url = %url, error = %e, "HTTP request failed");
            wire.transport_error(&e.to_string());
            return Err(types::ProviderError::RequestFailed(e.to_string()));
        }
    };
    wire.response(resp.status().as_u16(), resp.headers());
    if !resp.status().is_success() {
        return Err(types::ProviderError::Failed(
            failure::read_failure(brand, resp, wire).await,
        ));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| types::ProviderError::RequestFailed(e.to_string()))?;
    wire.body(&text);
    Ok(text)
}

/// Fetch a model list from an absolute OpenAI-compatible models URL
/// (`Authorization: Bearer`, `{"data":[{"id":..}]}` response shape).
///
/// Backs `catalog::ModelsEndpointStyle::OpenAiCompatible`: brands whose chat
/// protocol lives on one base URL while their models endpoint lives on
/// another (e.g. MiniMax's Anthropic-compatible chat base vs
/// `https://api.minimax.io/v1/models`). Capability flags come from the
/// catalog entry's provider-level `capabilities` because a bare id list
/// carries no per-model data.
pub(crate) async fn fetch_models_openai_compatible(
    brand: &str,
    url: &str,
    provider_id: &str,
    api_key: Option<&str>,
    caps: Option<&catalog::ProviderCaps>,
) -> Result<Vec<ModelInfo>, types::ProviderError> {
    tracing::info!(url = %url, "Fetching models from dedicated models endpoint");

    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(key) = api_key {
        headers.push(("Authorization".to_string(), format!("Bearer {}", key)));
    }

    let wire = wire_log::Scope::begin(&wire_log::Request {
        kind: "models",
        provider_id,
        protocol_id: "",
        model_id: None,
        run_id: None,
        session_id: None,
        method: "GET",
        url,
        headers: &headers,
        body: &serde_json::Value::Null,
    });

    let text = get_logged(&wire, brand, url, &headers).await?;
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| types::ProviderError::RequestFailed(e.to_string()))?;
    let (supports_tools, supports_images) = caps
        .map(|c| (c.supports_tools, c.supports_images))
        .unwrap_or((true, true));
    let models = body["data"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            Some(ModelInfo {
                id: id.clone(),
                display_name: id,
                supports_tools,
                supports_images,
            })
        })
        .collect();
    Ok(models)
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
    use crate::assistant::types::{AuthMode, ProviderConnection};

    fn connection(protocol_id: &str, provider_id: &str, model_id: &str) -> ProviderConnection {
        ProviderConnection {
            id: "conn".to_string(),
            name: "conn".to_string(),
            protocol_id: protocol_id.to_string(),
            provider_id: provider_id.to_string(),
            auth_mode: AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: "provider-connection::conn".to_string(),
            model_id: model_id.to_string(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
            env: Vec::new(),
        }
    }

    #[test]
    fn image_gate_resolves_default_and_explicit_models() {
        // Default model (empty id) is the commonest CLI case: must resolve via
        // the provider-level answer, not a (failing) per-id lookup.
        assert!(connection_supports_images(&connection(
            CLAUDE_CODE_PROVIDER_ID,
            CLAUDE_CODE_PROVIDER_ID,
            ""
        )));
        assert!(connection_supports_images(&connection(
            CODEX_PROVIDER_ID,
            CODEX_PROVIDER_ID,
            "  "
        )));
        assert!(!connection_supports_images(&connection(
            OPENCODE_PROVIDER_ID,
            OPENCODE_PROVIDER_ID,
            ""
        )));

        // Explicit known id uses that model's flag.
        assert!(connection_supports_images(&connection(
            CLAUDE_CODE_PROVIDER_ID,
            CLAUDE_CODE_PROVIDER_ID,
            "sonnet"
        )));
        // Unknown id for a uniformly-capable provider falls back to the
        // provider answer (still true), not false.
        assert!(connection_supports_images(&connection(
            CODEX_PROVIDER_ID,
            CODEX_PROVIDER_ID,
            "gpt-9-future"
        )));

        // API providers: best-effort per-protocol constant. Anthropic and
        // OpenAI chat models are vision-capable; their adapters send images.
        assert!(connection_supports_images(&connection(
            "anthropic",
            "anthropic",
            ""
        )));
        assert!(connection_supports_images(&connection(
            "openai", "openai", ""
        )));
        // Unknown provider → conservative false.
        assert!(!connection_supports_images(&connection("acme", "acme", "")));
    }

    #[test]
    fn image_gate_uses_catalog_brand_capabilities_before_protocol_defaults() {
        assert!(!connection_supports_images(&connection(
            "anthropic",
            "minimax",
            "MiniMax-M2"
        )));
        assert!(connection_supports_images(&connection(
            "anthropic",
            "anthropic",
            "claude-sonnet-4-5"
        )));
    }

    /// Each catalog entry's final URLs, as a connection saved from the catalog
    /// (base = `default_base_url`) requests them, against the endpoints the
    /// provider documents. A new entry must add its row here.
    #[test]
    fn catalog_entries_resolve_to_documented_endpoints() {
        use super::catalog::catalog_entries;
        use super::{anthropic, models_source, openai, ModelsSource};

        const DOCUMENTED: &[(&str, &str, Option<&str>)] = &[
            (
                "openai",
                "https://api.openai.com/v1/chat/completions",
                Some("https://api.openai.com/v1/models"),
            ),
            (
                "deepseek",
                "https://api.deepseek.com/v1/chat/completions",
                Some("https://api.deepseek.com/v1/models"),
            ),
            (
                "perplexity",
                "https://api.perplexity.ai/chat/completions",
                None,
            ),
            (
                "zai",
                "https://api.z.ai/api/paas/v4/chat/completions",
                Some("https://api.z.ai/api/paas/v4/models"),
            ),
            (
                "openrouter",
                "https://openrouter.ai/api/v1/chat/completions",
                Some("https://openrouter.ai/api/v1/models"),
            ),
            (
                "anthropic",
                "https://api.anthropic.com/v1/messages",
                Some("https://api.anthropic.com/v1/models"),
            ),
            (
                "minimax",
                "https://api.minimax.io/anthropic/v1/messages",
                Some("https://api.minimax.io/v1/models"),
            ),
            (
                "litellm",
                "http://localhost:4000/v1/chat/completions",
                Some("http://localhost:4000/v1/models"),
            ),
            (
                "ollama",
                "http://localhost:11434/v1/chat/completions",
                Some("http://localhost:11434/v1/models"),
            ),
            (
                "lmstudio",
                "http://localhost:1234/v1/chat/completions",
                Some("http://localhost:1234/v1/models"),
            ),
            (
                "vllm",
                "http://localhost:8000/v1/chat/completions",
                Some("http://localhost:8000/v1/models"),
            ),
        ];

        let mut checked = Vec::new();
        for entry in catalog_entries() {
            if entry.default_base_url.is_none() {
                continue; // custom-* entries: the user supplies the base
            }
            let mut conn = connection(&entry.protocol_id, &entry.id, "");
            conn.base_url = entry.default_base_url.clone();
            let (endpoint_url, chat_endpoint): (fn(&ProviderConnection, &str) -> String, _) =
                match entry.protocol_id.as_str() {
                    "anthropic" => (anthropic::endpoint_url, "messages"),
                    _ => (openai::endpoint_url, "chat/completions"),
                };
            let chat = endpoint_url(&conn, chat_endpoint);
            let models = match models_source(&conn, endpoint_url) {
                ModelsSource::Curated(_) => None,
                ModelsSource::OpenAiCompatible { url, .. } | ModelsSource::Endpoint(url) => {
                    Some(url)
                }
            };
            let (_, want_chat, want_models) = DOCUMENTED
                .iter()
                .find(|(id, _, _)| *id == entry.id)
                .unwrap_or_else(|| {
                    panic!("no documented endpoints for catalog entry {}", entry.id)
                });
            assert_eq!(chat, *want_chat, "{} chat URL", entry.id);
            assert_eq!(models.as_deref(), *want_models, "{} models URL", entry.id);
            checked.push(entry.id);
        }
        assert_eq!(checked.len(), DOCUMENTED.len(), "stale rows in DOCUMENTED");
    }

    /// A connection saved before brands existed has the `openai` brand; at
    /// Perplexity's base it must route like a Perplexity connection.
    #[test]
    fn legacy_openai_brand_at_perplexity_base_routes_as_perplexity() {
        use super::{models_source, openai, ModelsSource};

        let mut conn = connection("openai", "openai", "");
        conn.base_url = Some("https://api.perplexity.ai/".to_string());
        assert_eq!(
            openai::endpoint_url(&conn, "chat/completions"),
            "https://api.perplexity.ai/chat/completions"
        );
        assert!(matches!(
            models_source(&conn, openai::endpoint_url),
            ModelsSource::Curated(_)
        ));
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
