use serde::Serialize;
use tauri::State;

use crate::assistant::types::RunStatus;
use crate::config::ExecutionCapabilityConfig;
use crate::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSummary {
    pub total: usize,
    pub enabled: usize,
    pub running: usize,
    pub error: usize,
    pub idle: usize,
    pub disabled: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum FleetAgentStatus {
    Running,
    Ok,
    Warning,
    Error,
    Idle,
    Disabled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetRunEntry {
    pub status: RunStatus,
    pub started_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetAgentSnapshot {
    pub agent_id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub interval_minutes: u32,
    pub status: FleetAgentStatus,
    pub selected_mcp_server_ids: Vec<String>,
    pub selected_mcp_server_names: Vec<String>,
    pub selected_skill_ids: Vec<String>,
    pub provider_connection_ids: Vec<String>,
    pub provider_connection_names: Vec<String>,
    pub execution: ExecutionCapabilityConfig,
    pub session_id: Option<String>,
    pub last_started_at: Option<i64>,
    pub last_completed_at: Option<i64>,
    pub next_run_in_seconds: Option<u64>,
    pub last_run_status: Option<RunStatus>,
    pub last_error: Option<String>,
    pub last_message_preview: Option<String>,
    pub recent_run_statuses: Vec<FleetRunEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSnapshot {
    pub summary: FleetSummary,
    pub agents: Vec<FleetAgentSnapshot>,
}

#[tauri::command]
pub async fn fleet_get_snapshot(_state: State<'_, AppState>) -> Result<FleetSnapshot, String> {
    // The Fleet view no longer surfaces global agents — they don't exist as
    // a first-class concept anymore (agents are workspace-local). Kept for
    // frontend wire compatibility; agents are now enumerated per-workspace
    // via workspace_list.
    let items: Vec<FleetAgentSnapshot> = Vec::new();

    let summary = FleetSummary {
        total: 0,
        enabled: 0,
        running: 0,
        error: 0,
        idle: 0,
        disabled: 0,
    };

    Ok(FleetSnapshot {
        summary,
        agents: items,
    })
}

#[tauri::command]
pub async fn fleet_run_now(agent_id: String, state: State<'_, AppState>) -> Result<(), String> {
    // Verify the workspace-local agent exists and is enabled.
    let locators = state
        .workspace_index
        .read()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .locators_sorted();
    let mut found = None;
    for locator in locators {
        let Ok(config) = crate::config::workspace_config::load(&locator.root_path) else {
            continue;
        };
        if let Some(agent) = config.agents.iter().find(|agent| agent.id == agent_id) {
            found = Some(agent.enabled);
            break;
        }
    }
    match found {
        None => return Err(format!("Agent not found: {}", agent_id)),
        Some(false) => return Err("Agent is disabled. Enable it first.".to_string()),
        Some(true) => {}
    }

    let mut scheduler = state.scheduler.lock().await;
    if scheduler.force_ready(&agent_id) {
        Ok(())
    } else {
        Err("Agent is currently running or has no scheduler instance.".to_string())
    }
}
