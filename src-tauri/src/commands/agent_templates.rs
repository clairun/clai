//! Bundled agent-template commands.

use serde::Serialize;
use tauri::State;

use crate::config::bundled::{self, BundledAgentTemplate};
use crate::config::ExecutionCapabilityConfig;
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTemplateResponse {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_skill_ids: Vec<String>,
    pub default_execution: ExecutionCapabilityConfig,
}

#[tauri::command]
pub fn agent_templates_list(
    state: State<'_, AppState>,
) -> Result<Vec<AgentTemplateResponse>, String> {
    let config = {
        let config_manager = state
            .config_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        config_manager.get()
    };

    bundled::load_bundled_agent_templates()?
        .into_iter()
        .map(|template| resolve_template(template, &config))
        .collect()
}

fn resolve_template(
    template: BundledAgentTemplate,
    config: &crate::config::ClaiConfig,
) -> Result<AgentTemplateResponse, String> {
    let mut default_skill_ids = Vec::with_capacity(template.default_skill_slugs.len());
    for slug in &template.default_skill_slugs {
        let Some(skill_id) = bundled::resolve_bundled_skill_id(slug, config) else {
            return Err(format!(
                "Bundled skill source is not registered; cannot resolve '{}'.",
                slug
            ));
        };
        default_skill_ids.push(skill_id);
    }

    Ok(AgentTemplateResponse {
        id: template.id,
        name: template.name,
        description: template.description,
        default_skill_ids,
        default_execution: template.default_execution,
    })
}
