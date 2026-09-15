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
//!   `openai` | `anthropic` | `claude-code` | `codex` | `opencode`.
//! - **`id`** (== a connection's brand `provider_id`) — the catalog/brand key
//!   (`openrouter`, `ollama`, `minimax`, …), used for the logo, display name, preset
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
    /// Live model listing at an absolute OpenAI-compatible URL (Bearer auth,
    /// `{"data":[{"id":..}]}` response) hosted apart from the chat `base_url`
    /// — e.g. MiniMax pairs an Anthropic-compatible chat base with an
    /// OpenAI-style models endpoint.
    OpenAiCompatible { url: String },
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
    /// Frontend asset path, e.g. `provider-catalog/openrouter.svg`.
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
    model_with_capabilities(id, true, true)
}

fn model_with_capabilities(id: &str, supports_tools: bool, supports_images: bool) -> ModelInfo {
    ModelInfo {
        id: id.to_string(),
        display_name: id.to_string(),
        supports_tools,
        supports_images,
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
#[expect(
    clippy::too_many_lines,
    reason = "lint debt: 152 lines against a 100-line budget; split it, do not raise the budget"
)]
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
            "deepseek",
            "DeepSeek",
            "DeepSeek chat and reasoning models.",
            "https://api.deepseek.com/v1",
            "https://platform.deepseek.com/api_keys",
            &["deepseek-chat", "deepseek-reasoner"],
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
        // The current Anthropic lineup, by alias. Despite
        // `models_endpoint_style: Standard`, nothing probes the live endpoint
        // on its own: this is the *default* model quick-pick for every
        // Anthropic connection, on create and on edit alike, and it is
        // replaced only once the user presses "Load models"
        // (`AssistantProviderSettings.tsx`, `quickPickModels`). A retired id
        // here is therefore offered to every user, not only keyless ones.
        // Source: https://platform.claude.com/docs/en/about-claude/model-deprecations
        curated_models: [
            "claude-fable-5-1",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-haiku-4-5",
        ]
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
        curated_models: vec![model_with_capabilities("MiniMax-M2", true, false)],
        docs_url: Some("https://www.minimax.io/platform".to_string()),
        extra_headers: Vec::new(),
        models_endpoint_style: ModelsEndpointStyle::OpenAiCompatible {
            url: "https://api.minimax.io/v1/models".to_string(),
        },
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
    fn anthropic_curates_one_model_per_live_claude_family() {
        let e = get_entry("anthropic").expect("anthropic present");
        let ids: Vec<&str> = e.curated_models.iter().map(|m| m.id.as_str()).collect();
        for family in ["fable", "sonnet", "opus", "haiku"] {
            assert!(
                ids.iter().any(|id| id.contains(family)),
                "curated fallback is missing the {family} family: {ids:?}"
            );
        }
    }

    /// Model ids Anthropic has already retired, in the forms a caller might
    /// plausibly have typed or copied: each dated snapshot, plus the aliases
    /// Anthropic published for it. Not an exhaustive list of every id ever
    /// issued — Bedrock and Vertex spellings are omitted because this catalog
    /// entry is `base_url_locked` to `https://api.anthropic.com`.
    ///
    /// Matched by equality, never by prefix: `claude-opus-4-8` and
    /// `claude-sonnet-4-6` are active and both have a retired id as a prefix.
    ///
    /// Dates from
    /// <https://platform.claude.com/docs/en/about-claude/model-deprecations>,
    /// checked 2026-09-15.
    const RETIRED_ANTHROPIC_MODEL_IDS: &[&str] = &[
        "claude-opus-4-1",
        "claude-opus-4-1-20250805",
        "claude-opus-4",
        "claude-opus-4-0",
        "claude-opus-4-20250514",
        "claude-sonnet-4",
        "claude-sonnet-4-0",
        "claude-sonnet-4-20250514",
        "claude-3-7-sonnet",
        "claude-3-7-sonnet-latest",
        "claude-3-7-sonnet-20250219",
        "claude-3-5-sonnet",
        "claude-3-5-sonnet-latest",
        "claude-3-5-sonnet-20241022",
        "claude-3-5-sonnet-20240620",
        "claude-3-5-haiku",
        "claude-3-5-haiku-latest",
        "claude-3-5-haiku-20241022",
        "claude-3-haiku",
        "claude-3-haiku-20240307",
    ];

    /// A revert guard, not a staleness detector. It fails if someone puts a
    /// *known* retired id back into a curated list; it cannot notice a model
    /// retiring in the future, because the list above only changes when a
    /// human edits it. The nearest such date is `claude-haiku-4-5`, curated
    /// above and retiring no sooner than 2026-10-15. Closing that hole would
    /// need a live call to Anthropic, which needs a key the app does not have
    /// at catalog-build time — so a hand-maintained list is the honest answer.
    #[test]
    fn curated_anthropic_ids_are_not_on_the_known_retired_list() {
        let anthropic_entries = catalog_entries()
            .into_iter()
            .filter(|e| e.protocol_id == "anthropic")
            .collect::<Vec<_>>();
        // Name the entry this exists for, so narrowing the filter cannot make
        // the test pass by checking nothing.
        assert!(
            anthropic_entries.iter().any(|e| e.id == "anthropic"),
            "the Anthropic brand entry is not in the checked set"
        );
        for e in anthropic_entries {
            for m in &e.curated_models {
                assert!(
                    !RETIRED_ANTHROPIC_MODEL_IDS.contains(&m.id.as_str()),
                    "{} curates retired model {}",
                    e.id,
                    m.id
                );
            }
        }
    }

    #[test]
    fn openrouter_carries_attribution_headers() {
        let e = get_entry("openrouter").expect("openrouter present");
        assert!(e.extra_headers.iter().any(|(k, _)| k == "HTTP-Referer"));
        assert!(e.extra_headers.iter().any(|(k, _)| k == "X-Title"));
    }

    #[test]
    fn minimax_lists_models_from_dedicated_openai_style_endpoint() {
        let e = get_entry("minimax").expect("minimax present");
        assert!(matches!(
            e.models_endpoint_style,
            ModelsEndpointStyle::OpenAiCompatible { ref url }
                if url == "https://api.minimax.io/v1/models"
        ));
        assert!(e
            .capabilities
            .as_ref()
            .is_some_and(|caps| !caps.supports_images));
        assert!(e.curated_models.iter().all(|m| !m.supports_images));
    }

    #[test]
    fn get_entry_returns_none_for_unknown_brand() {
        assert!(get_entry("definitely-not-a-provider").is_none());
        assert!(get_entry("openai").is_some());
    }

    #[test]
    fn hosted_endpoints_are_locked() {
        let e = get_entry("openai").expect("openai present");
        assert!(e.base_url_locked);
        assert!(e.requires_api_key);
    }

    #[test]
    fn long_tail_hosted_providers_are_not_bundled() {
        for id in [
            "cerebras",
            "fireworks",
            "gemini",
            "groq",
            "mistral",
            "together",
            "xai",
        ] {
            assert!(get_entry(id).is_none(), "{id} should not be in the catalog");
        }
    }
}
