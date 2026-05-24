//! Agent initialization.
//!
//! Agents are workspace-local (`workspace_agents` DB table). Scheduling for
//! them is populated by `populate_scheduler_from_workspace_agents` below,
//! which runs after the DB pool is ready.

use crate::agents::{AgentDefinition, SharedScheduler};
use crate::auth::TokenStorage;
use crate::config::{
    workspace_config, AgentConfig, ConfigManager, ExecutionCapabilityConfig, ShellAccessMode,
};
use crate::AppState;

#[allow(dead_code)]
fn agent_config_to_definition(config: &AgentConfig) -> AgentDefinition {
    AgentDefinition::new(
        &config.id,
        &config.name,
        (config.interval_minutes as u64) * 60 * 1000,
    )
    .with_description(&config.description)
    .with_tools(config.required_tools())
}

/// No-op kept so the synchronous lib.rs setup path stays untouched. The real
/// scheduler population now happens in `populate_scheduler_from_workspace_agents`
/// once the DB pool is initialized.
pub fn initialize_scheduler(
    _scheduler: &SharedScheduler,
    _config_manager: &ConfigManager,
    _token_storage: &TokenStorage,
) {
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
        if !locator.schedule_enabled {
            continue;
        }
        let Ok(config) = workspace_config::load(&locator.root_path) else {
            continue;
        };
        // The workspace schedule fires the workspace's default (manager) agent.
        let Some(agent) = config
            .agents
            .iter()
            .find(|agent| agent.id == config.default_agent_id)
        else {
            continue;
        };

        let execution: ExecutionCapabilityConfig = agent.execution.clone();
        let mut tools: Vec<&'static str> = vec!["netdata", "dashboard", "tabs", "fs"];
        if !matches!(execution.shell.mode, ShellAccessMode::Off) {
            tools.push("bash");
        }
        if execution.web.enabled {
            tools.push("web");
        }
        let definition = AgentDefinition::new(
            &agent.id,
            &agent.name,
            (config.schedule.interval_minutes as u64).max(1) * 60 * 1000,
        )
        .with_tools(tools);
        sched.register_definition(definition);
        if agent.enabled {
            let instance_id = sched.create_instance(&agent.id, "", "");
            // Paused workspaces still get a scheduler instance (so the live
            // pause/resume toggle can flip it without re-registering), but the
            // instance is created disabled so the runner ignores it until
            // resumed.
            if config.schedule.paused {
                if let Some(instance_id) = instance_id {
                    sched.set_instance_enabled(&instance_id, false);
                }
            }
        }
    }
}

/// Clears all agent instances.
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
        AgentConfig::new("Test Agent".to_string(), "Test description".to_string(), 5)
    }

    #[test]
    fn test_agent_config_to_definition() {
        let agent = create_test_agent_config();
        let definition = agent_config_to_definition(&agent);

        assert_eq!(definition.id, agent.id);
        assert_eq!(definition.name, "Test Agent");
        assert_eq!(definition.interval_ms, 5 * 60 * 1000);
        assert_eq!(
            definition.required_tools,
            vec!["netdata", "dashboard", "tabs", "fs"]
        );
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
