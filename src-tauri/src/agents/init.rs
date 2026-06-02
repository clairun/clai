//! Agent initialization.
//!
//! Agents are workspace-local (`workspace_agents` DB table). Scheduling for
//! them is populated by `populate_scheduler_from_workspace_agents` below,
//! which runs after the DB pool is ready.

use crate::agents::{AgentDefinition, SharedScheduler};
use crate::config::{
    workspace_config, AgentConfig, ConfigManager, ExecutionCapabilityConfig, ShellAccessMode,
};
use crate::AppState;

#[allow(dead_code)]
fn agent_config_to_definition(config: &AgentConfig) -> AgentDefinition {
    AgentDefinition::new(&config.id, &config.name)
        .with_description(&config.description)
        .with_tools(config.required_tools())
}

/// No-op kept so the synchronous lib.rs setup path stays untouched. The real
/// scheduler population now happens in `populate_scheduler_from_workspace_agents`
/// once the DB pool is initialized.
pub fn initialize_scheduler(_scheduler: &SharedScheduler, _config_manager: &ConfigManager) {
    // intentionally empty
}

/// Populates the scheduler with scheduled workspace-local agents from workspace configs.
///
/// Called after workspace storage is initialized. Reads every workspace
/// `config.json`, registers scheduled agent definitions, and creates an
/// instance for each enabled agent.
pub async fn populate_scheduler_from_workspace_agents(
    scheduler: &SharedScheduler,
    state: &AppState,
) {
    let locators = match state.workspace_index.read() {
        Ok(index) => index.locators_sorted(),
        Err(e) => {
            tracing::warn!(
                "Failed to read workspace index for scheduler population: {}",
                e
            );
            return;
        }
    };

    let mut sched = scheduler.lock().await;
    for locator in locators {
        let Ok(config) = workspace_config::load(&locator.root_path) else {
            continue;
        };
        apply_workspace_schedule(&mut sched, &config);
    }
}

/// Reconcile the scheduler's view of a single workspace with its current
/// `WorkspaceConfig`. Removes any prior instance for the workspace's
/// default agent, then re-registers if `schedule.enabled`. Idempotent —
/// callable from startup (via `populate_scheduler_from_workspace_agents`)
/// and from the `workspace_set_schedule` / `workspace_set_schedule_paused`
/// Tauri commands so live state always tracks the file.
pub fn apply_workspace_schedule(
    sched: &mut crate::agents::scheduler::Scheduler,
    config: &crate::config::WorkspaceConfig,
) {
    // The workspace schedule fires the workspace's default (manager) agent.
    let Some(agent) = config
        .agents
        .iter()
        .find(|agent| agent.id == config.default_agent_id)
    else {
        return;
    };

    // Always start clean: drop any prior instance(s) for this agent. Cheap
    // when there is none; safe when there is one of either polarity.
    sched.remove_instances_for_agent(&agent.id);

    if !config.schedule.enabled {
        return;
    }

    let execution: ExecutionCapabilityConfig = agent.execution.clone();
    let mut tools: Vec<&'static str> = vec!["dashboard", "tabs", "fs"];
    if !matches!(execution.shell.mode, ShellAccessMode::Off) {
        tools.push("bash");
    }
    if execution.web.enabled {
        tools.push("web");
    }
    let definition = AgentDefinition::new(&agent.id, &agent.name).with_tools(tools);
    sched.register_definition(definition);

    if agent.enabled {
        let instance_id = sched.create_instance(&agent.id, "", "");
        // Paused workspaces still get an instance (so pause/resume can flip
        // it without re-registering), but the instance starts disabled so
        // the runner skips it until resumed.
        if let Some(instance_id) = instance_id {
            // Seed the in-memory `next_run_at` from the persisted
            // wall-clock value. Without this, every scheduled workspace
            // would fire immediately on app start regardless of when it
            // was actually due — the `Instant` `next_run_at` doesn't
            // survive process restart.
            sched.set_instance_next_run_at(&instance_id, config.schedule.next_run_at_unix_ms);
            if config.schedule.paused {
                sched.set_instance_enabled(&instance_id, false);
            }
        }
    }
}

/// Clears all agent instances.
#[allow(dead_code)]
pub async fn clear_all_instances(scheduler: &SharedScheduler) {
    let mut scheduler = scheduler.lock().await;

    let instance_ids: Vec<String> = scheduler
        .all_instances()
        .map(|i| i.instance_id.clone())
        .collect();

    for instance_id in instance_ids {
        scheduler.remove_instance(&instance_id);
    }
}

/// Creates a scheduler instance for a specific agent.
#[allow(dead_code)]
pub async fn create_instance_for_agent(scheduler: &SharedScheduler, agent_id: &str) {
    let mut scheduler = scheduler.lock().await;
    scheduler.create_instance(agent_id, "", "");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::create_shared_scheduler;

    fn create_test_agent_config() -> AgentConfig {
        AgentConfig::new(
            "Test Agent".to_string(),
            "Test description".to_string(),
            crate::config::workspace_config::ScheduleKind::Interval {
                interval_minutes: 5,
            },
        )
    }

    #[test]
    fn test_agent_config_to_definition() {
        let agent = create_test_agent_config();
        let definition = agent_config_to_definition(&agent);

        assert_eq!(definition.id, agent.id);
        assert_eq!(definition.name, "Test Agent");
        assert_eq!(definition.required_tools, vec!["dashboard", "tabs", "fs"]);
    }

    #[tokio::test]
    async fn test_create_and_remove_instance_for_agent() {
        let scheduler = create_shared_scheduler();

        let agent = create_test_agent_config();
        {
            let mut s = scheduler.lock().await;
            s.register_definition(agent_config_to_definition(&agent));
        }

        create_instance_for_agent(&scheduler, &agent.id).await;

        {
            let s = scheduler.lock().await;
            assert_eq!(s.instance_count(), 1);
            let instance_id = format!("{}::", agent.id);
            let instance = s.get_instance(&instance_id);
            assert!(instance.is_some());
        }

        {
            let mut s = scheduler.lock().await;
            s.remove_instances_for_agent(&agent.id);
        }

        {
            let s = scheduler.lock().await;
            assert_eq!(s.instance_count(), 0);
        }
    }

    #[tokio::test]
    async fn test_clear_all_instances() {
        let scheduler = create_shared_scheduler();

        let agent = create_test_agent_config();
        {
            let mut s = scheduler.lock().await;
            s.register_definition(agent_config_to_definition(&agent));
        }

        create_instance_for_agent(&scheduler, &agent.id).await;

        {
            let s = scheduler.lock().await;
            assert_eq!(s.instance_count(), 1);
        }

        clear_all_instances(&scheduler).await;

        {
            let s = scheduler.lock().await;
            assert_eq!(s.instance_count(), 0);
        }
    }
}
