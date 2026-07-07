//! Predefined provider catalog.
//!
//! A bundled, static list of well-known model providers (hosted SaaS,
//! self-hosted/open-source, and the CLI providers) so a user can pick a
//! provider from a list — with logo, endpoint, and curated models prefilled —
//! instead of typing an endpoint by hand. Mirrors the MCP server catalog
//! (`crate::mcp::oauth::catalog_entries`).
//!
//! Two identifiers are kept deliberately separate (see the provider-catalog
//! design doc):
//!
//! - **`protocol_id`** — the wire/execution backend that drives dispatch
//!   (`resolve_adapter` / `is_cli_provider` / `CliProviderRuntime`). One of
//!   `openai` | `anthropic` | `claude` | `codex` | `opencode` | `gemini`.
//! - **`id`** (== a connection's brand `provider_id`) — the catalog/brand key
//!   (`groq`, `mistral`, `ollama`, …), used for the logo, display name, preset
//!   memory, and per-provider quirk data.
//!
//! Provider divergence is expressed as **data on the entry** (`extra_headers`,
//! `models_endpoint_style`, `capabilities`) consumed by the generic adapters —
//! so a new provider never needs a new adapter type.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::assistant::types::ModelInfo;

/// Where a catalog entry sits in the picker (also drives form defaults).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ProviderCategory {
    /// Hosted SaaS with a fixed endpoint (base_url locked; advanced override only).
    Hosted,
    /// Self-hosted / local (base_url editable; API key optional).
    SelfHosted,
    /// CLI-backed provider (auto-detected; behavior unchanged).
    Cli,
    /// Generic fallback (fully editable base_url + key).
    Custom,
}

/// How to list models for the provider (quirk-as-data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum ModelsEndpointStyle {
    /// Standard `<base>/models` (OpenAI-compatible) — the default.
    Standard,
    /// No live model listing; use `curated_models` only.
    None,
}

/// Provider-level capability defaults, used when a live model list is
/// unavailable/thin (keyless or offline providers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ProviderCaps {
    pub supports_tools: bool,
    pub supports_images: bool,
}

/// A single predefined provider preset.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct ProviderCatalogEntry {
    /// Brand/catalog id — becomes the connection's `provider_id`.
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub category: ProviderCategory,
    /// Wire protocol adapter key — becomes the connection's `protocol_id`.
    pub protocol_id: String,
    pub default_base_url: Option<String>,
    /// Hosted SaaS: endpoint fixed (advanced-override only). Self-hosted/custom: editable.
    pub base_url_locked: bool,
    /// `false` for keyless self-hosted providers (ollama / lmstudio / vllm).
    pub requires_api_key: bool,
    /// Frontend asset path, e.g. `provider-catalog/groq.svg`.
    pub logo_asset: String,
    /// Fallback model list when a live `/v1/models` probe is unavailable.
    pub curated_models: Vec<ModelInfo>,
    /// "Where do I get my API key?" link.
    pub docs_url: Option<String>,

    // --- extensibility: quirks as DATA, consumed by the generic adapters ---
    /// Extra request headers (e.g. OpenRouter attribution).
    pub extra_headers: Vec<(String, String)>,
    pub models_endpoint_style: ModelsEndpointStyle,
    /// Capability defaults when the models endpoint is thin/absent.
    pub capabilities: Option<ProviderCaps>,
}

fn model(id: &str) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        display_name: id.to_string(),
        supports_tools: true,
        supports_images: true,
    }
}

/// Builder for the common hosted OpenAI-compatible entry (locked endpoint, key
/// required, standard `/models`).
#[allow(clippy::too_many_arguments)]
fn hosted_openai(
    id: &str,
    display_name: &str,
    description: &str,
    base_url: &str,
    docs_url: &str,
    curated: &[&str],
) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        id: id.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        category: ProviderCategory::Hosted,
        protocol_id: "openai".to_string(),
        default_base_url: Some(base_url.to_string()),
        base_url_locked: true,
        requires_api_key: true,
        logo_asset: format!("provider-catalog/{id}.svg"),
        curated_models: curated.iter().map(|m| model(m)).collect(),
        docs_url: Some(docs_url.to_string()),
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::Standard,
        capabilities: None,
    }
}

/// The bundled provider catalog (v1). See the provider-catalog design doc §5.
pub fn catalog_entries() -> Vec<ProviderCatalogEntry> {
    let mut entries = vec![
        // --- Hosted, OpenAI-compatible ---
        hosted_openai(
            "openai",
            "OpenAI",
            "GPT models from OpenAI.",
            "https://api.openai.com/v1",
            "https://platform.openai.com/api-keys",
            &["gpt-5.1", "gpt-5.1-mini", "gpt-4.1", "o4-mini"],
        ),
        hosted_openai(
            "groq",
            "Groq",
            "Ultra-low-latency inference on open models.",
            "https://api.groq.com/openai/v1",
            "https://console.groq.com/keys",
            &[
                "llama-3.3-70b-versatile",
                "llama-3.1-8b-instant",
                "moonshotai/kimi-k2-instruct",
            ],
        ),
        hosted_openai(
            "mistral",
            "Mistral",
            "Mistral's open and commercial models.",
            "https://api.mistral.ai/v1",
            "https://console.mistral.ai/api-keys",
            &["mistral-large-latest", "mistral-small-latest"],
        ),
        hosted_openai(
            "deepseek",
            "DeepSeek",
            "DeepSeek chat and reasoning models.",
            "https://api.deepseek.com/v1",
            "https://platform.deepseek.com/api_keys",
            &["deepseek-chat", "deepseek-reasoner"],
        ),
        hosted_openai(
            "xai",
            "xAI (Grok)",
            "Grok models from xAI.",
            "https://api.x.ai/v1",
            "https://console.x.ai",
            &["grok-4", "grok-4-fast"],
        ),
        hosted_openai(
            "together",
            "Together AI",
            "Open models at scale (Llama, Qwen, DeepSeek, …).",
            "https://api.together.xyz/v1",
            "https://api.together.ai/settings/api-keys",
            &[],
        ),
        hosted_openai(
            "fireworks",
            "Fireworks AI",
            "Fast open-model inference.",
            "https://api.fireworks.ai/inference/v1",
            "https://fireworks.ai/account/api-keys",
            &[],
        ),
        hosted_openai(
            "cerebras",
            "Cerebras",
            "Wafer-scale inference for open models.",
            "https://api.cerebras.ai/v1",
            "https://cloud.cerebras.ai",
            &["llama-3.3-70b", "qwen-3-235b-a22b-instruct-2507"],
        ),
        hosted_openai(
            "perplexity",
            "Perplexity",
            "Sonar models with built-in web search.",
            "https://api.perplexity.ai",
            "https://www.perplexity.ai/settings/api",
            &["sonar", "sonar-pro", "sonar-reasoning"],
        ),
        hosted_openai(
            "gemini",
            "Google Gemini",
            "Gemini models via the OpenAI-compatible endpoint.",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://aistudio.google.com/apikey",
            &["gemini-2.5-pro", "gemini-2.5-flash"],
        ),
        hosted_openai(
            "zai",
            "Z.ai (GLM)",
            "Zhipu GLM models via the OpenAI-compatible endpoint.",
            "https://api.z.ai/api/paas/v4",
            "https://z.ai/manage-apikey/apikey-list",
            &["glm-4.6", "glm-4.5-air"],
        ),
    ];

    // OpenRouter — hosted OpenAI-compatible + attribution headers (quirk-as-data).
    let mut openrouter = hosted_openai(
        "openrouter",
        "OpenRouter",
        "One key, hundreds of models routed across providers.",
        "https://openrouter.ai/api/v1",
        "https://openrouter.ai/keys",
        &[],
    );
    openrouter.extra_headers = vec![
        ("HTTP-Referer".to_string(), "https://clai.run".to_string()),
        ("X-Title".to_string(), "CLAI".to_string()),
    ];
    entries.push(openrouter);

    // --- Hosted, Anthropic-compatible ---
    entries.push(ProviderCatalogEntry {
        id: "anthropic".to_string(),
        display_name: "Anthropic".to_string(),
        description: "Claude models from Anthropic.".to_string(),
        category: ProviderCategory::Hosted,
        protocol_id: "anthropic".to_string(),
        default_base_url: Some("https://api.anthropic.com".to_string()),
        base_url_locked: true,
        requires_api_key: true,
        logo_asset: "provider-catalog/anthropic.svg".to_string(),
        curated_models: ["claude-sonnet-4-5", "claude-opus-4-1", "claude-haiku-4-5"]
            .iter()
            .map(|m| model(m))
            .collect(),
        docs_url: Some("https://console.anthropic.com/settings/keys".to_string()),
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::Standard,
        capabilities: None,
    });
    // MiniMax — Anthropic-compatible endpoint (user preference; international, not the CN variant).
    entries.push(ProviderCatalogEntry {
        id: "minimax".to_string(),
        display_name: "MiniMax".to_string(),
        description: "MiniMax models via the Anthropic-compatible endpoint.".to_string(),
        category: ProviderCategory::Hosted,
        protocol_id: "anthropic".to_string(),
        default_base_url: Some("https://api.minimax.io/anthropic".to_string()),
        base_url_locked: true,
        requires_api_key: true,
        logo_asset: "provider-catalog/minimax.svg".to_string(),
        curated_models: ["MiniMax-M2"].iter().map(|m| model(m)).collect(),
        docs_url: Some("https://www.minimax.io/platform".to_string()),
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::None,
        capabilities: Some(ProviderCaps {
            supports_tools: true,
            supports_images: false,
        }),
    });

    // --- Self-hosted / open source (base_url editable, key optional) ---
    entries.push(self_hosted(
        "litellm",
        "LiteLLM",
        "Self-hosted proxy exposing many providers via one OpenAI-compatible endpoint.",
        "http://localhost:4000",
        true, // LiteLLM commonly uses a master key
        "https://docs.litellm.ai/docs/simple_proxy",
    ));
    entries.push(self_hosted(
        "ollama",
        "Ollama",
        "Run open models locally.",
        "http://localhost:11434/v1",
        false,
        "https://ollama.com",
    ));
    entries.push(self_hosted(
        "lmstudio",
        "LM Studio",
        "Local model runner with an OpenAI-compatible server.",
        "http://localhost:1234/v1",
        false,
        "https://lmstudio.ai",
    ));
    entries.push(self_hosted(
        "vllm",
        "vLLM",
        "High-throughput self-hosted inference server.",
        "http://localhost:8000/v1",
        false,
        "https://docs.vllm.ai",
    ));

    // --- Generic fallbacks (always present) ---
    entries.push(ProviderCatalogEntry {
        id: "custom-openai".to_string(),
        display_name: "Custom (OpenAI-compatible)".to_string(),
        description: "Any OpenAI-compatible endpoint.".to_string(),
        category: ProviderCategory::Custom,
        protocol_id: "openai".to_string(),
        default_base_url: None,
        base_url_locked: false,
        requires_api_key: true,
        logo_asset: "provider-catalog/custom-openai.svg".to_string(),
        curated_models: Vec::new(),
        docs_url: None,
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::Standard,
        capabilities: None,
    });
    entries.push(ProviderCatalogEntry {
        id: "custom-anthropic".to_string(),
        display_name: "Custom (Anthropic-compatible)".to_string(),
        description: "Any Anthropic-compatible endpoint.".to_string(),
        category: ProviderCategory::Custom,
        protocol_id: "anthropic".to_string(),
        default_base_url: None,
        base_url_locked: false,
        requires_api_key: true,
        logo_asset: "provider-catalog/custom-anthropic.svg".to_string(),
        curated_models: Vec::new(),
        docs_url: None,
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::Standard,
        capabilities: None,
    });

    entries
}

fn self_hosted(
    id: &str,
    display_name: &str,
    description: &str,
    base_url: &str,
    requires_api_key: bool,
    docs_url: &str,
) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        id: id.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        category: ProviderCategory::SelfHosted,
        protocol_id: "openai".to_string(),
        default_base_url: Some(base_url.to_string()),
        base_url_locked: false,
        requires_api_key,
        logo_asset: format!("provider-catalog/{id}.svg"),
        curated_models: Vec::new(),
        docs_url: Some(docs_url.to_string()),
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::Standard,
        capabilities: None,
    }
}

/// Look up a catalog entry by its brand id.
#[allow(dead_code)] // wired by the probe (stage 3) + quirk plumbing (stage 4)
pub fn get_entry(provider_id: &str) -> Option<ProviderCatalogEntry> {
    catalog_entries().into_iter().find(|e| e.id == provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_non_empty_and_ids_unique() {
        let entries = catalog_entries();
        assert!(entries.len() > 10);
        let mut ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate catalog ids");
    }

    #[test]
    fn every_entry_uses_a_real_protocol() {
        for e in catalog_entries() {
            assert!(
                matches!(e.protocol_id.as_str(), "openai" | "anthropic"),
                "entry {} has unknown protocol_id {}",
                e.id,
                e.protocol_id
            );
        }
    }

    #[test]
    fn keyless_self_hosted_do_not_require_a_key() {
        for id in ["ollama", "lmstudio", "vllm"] {
            let e = get_entry(id).expect("entry present");
            assert!(!e.requires_api_key, "{id} should be keyless");
            assert!(!e.base_url_locked, "{id} base_url should be editable");
        }
    }

    #[test]
    fn openrouter_carries_attribution_headers() {
        let e = get_entry("openrouter").expect("openrouter present");
        assert!(e.extra_headers.iter().any(|(k, _)| k == "HTTP-Referer"));
        assert!(e.extra_headers.iter().any(|(k, _)| k == "X-Title"));
    }

    #[test]
    fn hosted_endpoints_are_locked() {
        let e = get_entry("openai").expect("openai present");
        assert!(e.base_url_locked);
        assert!(e.requires_api_key);
    }
}
