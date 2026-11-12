//! Agent initialization.
//!
//! Agents are workspace-local (`workspace_agents` DB table). Scheduling for
//! them is populated by `populate_scheduler_from_workspace_agents` below,
//! which runs after the DB pool is ready.

use crate::agents::{AgentDefinition, SharedScheduler};
use crate::config::{workspace_config, ConfigManager, ExecutionCapabilityConfig, ShellAccessMode};
use crate::AppState;

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

    // Restore a persisted global "pause all" overlay. This sits on top of the
    // per-workspace instance state seeded above: while paused the runner skips
    // every tick, and resuming leaves each workspace's individual pause state
    // (set via apply_workspace_schedule) intact.
    let globally_paused = state
        .config_manager
        .lock()
        .map(|cm| cm.get().scheduler_paused)
        .unwrap_or(false);
    if globally_paused {
        sched.pause("Paused by user".to_string());
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
    // The workspace schedule fires the workspace's own Main agent. A shared
    // definition never becomes a scheduler key: the same teammate assigned in
    // five workspaces would collide on one instance.
    let Some(agent) = config.main_agent.as_ref() else {
        return;
    };

    // Always start clean: drop any prior instance(s) for this agent. Cheap
    // when there is none; safe when there is one of either polarity.
    sched.remove_instances_for_agent(&agent.id);

    if !config.schedule.enabled {
        return;
    }

    let execution: ExecutionCapabilityConfig = agent.execution.clone();
    let mut tools: Vec<&'static str> = vec!["dashboard", "tabs"];
    if !matches!(execution.shell.mode, ShellAccessMode::Off) {
        tools.push("bash");
    }
    if execution.web.enabled {
        tools.push("web");
    }
    let definition = AgentDefinition::new(&agent.id, &agent.name)
        .with_workspace_id(&config.id)
        .with_tools(tools);
    sched.register_definition(definition);

    if agent.enabled {
        let instance_id = sched.create_instance(&agent.id);
        // Paused workspaces still get an instance (so pause/resume can flip
        // it without re-registering), but the instance starts disabled so
        // the runner skips it until resumed.
        if let Some(instance_id) = instance_id {
            // Seed the in-memory `next_run_at` from the persisted
            // wall-clock value. Without this, every scheduled workspace
            // would fire immediately on app start regardless of when it
            // was actually due — the `Instant` `next_run_at` doesn't
            // survive process restart.
            let now = chrono::Utc::now().timestamp_millis();
            sched.set_instance_next_run_at(&instance_id, seeded_next_run_at(&config.schedule, now));
            if config.schedule.paused {
                sched.set_instance_enabled(&instance_id, false);
            }
        }
    }
}

/// The persisted next-run target, except that a future cron target the
/// schedule doesn't fire at is recomputed from `now`: one stored before a
/// fix to cron parsing (such as the day-of-week numbering) may land on
/// the wrong day. Valid targets are kept even when `now` is behind them,
/// so a slow clock can't add a run. Past-due targets are kept so the
/// missed run still fires, and any error keeps the stored value.
fn seeded_next_run_at(schedule: &workspace_config::WorkspaceSchedule, now: i64) -> Option<i64> {
    let stored = schedule.next_run_at_unix_ms?;
    let workspace_config::ScheduleKind::Cron {
        expression,
        timezone,
    } = &schedule.kind
    else {
        return Some(stored);
    };
    let valid = crate::agents::schedule::cron_fires_at(expression, timezone, stored);
    if stored <= now || valid.unwrap_or(true) {
        return Some(stored);
    }
    Some(crate::agents::schedule::compute_next_run_at(&schedule.kind, now).unwrap_or(stored))
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
    scheduler.create_instance(agent_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::create_shared_scheduler;
    use crate::agents::types::AgentInstance;
    use crate::config::workspace_config::{ScheduleKind, WorkspaceSchedule};

    fn create_test_agent_config() -> crate::config::AgentConfig {
        crate::config::AgentConfig::new(
            "Test Agent".to_string(),
            "Test description".to_string(),
            crate::config::workspace_config::ScheduleKind::Interval {
                interval_minutes: 5,
            },
        )
    }

    fn schedule(kind: ScheduleKind, next_run_at_unix_ms: Option<i64>) -> WorkspaceSchedule {
        WorkspaceSchedule {
            enabled: true,
            paused: false,
            kind,
            next_run_at_unix_ms,
        }
    }

    fn cron(expression: &str) -> ScheduleKind {
        ScheduleKind::Cron {
            expression: expression.to_string(),
            timezone: "UTC".to_string(),
        }
    }

    /// Unix ms of November `day`, 2026, `hour:minute` UTC.
    fn utc_ms(day: u32, hour: u32, minute: u32) -> i64 {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, day, hour, minute, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn seeded_next_run_at_recomputes_future_cron_targets() {
        // Sunday 2026-11-08: "Mondays 9am" stored as Sunday 9am by the
        // old day-of-week numbering is moved to Monday 9am.
        let now = utc_ms(8, 0, 0);
        let mondays = schedule(cron("0 9 * * 1"), Some(utc_ms(8, 9, 0)));
        assert_eq!(seeded_next_run_at(&mondays, now), Some(utc_ms(9, 9, 0)));

        let correct = schedule(cron("0 9 * * 1"), Some(utc_ms(9, 9, 0)));
        assert_eq!(seeded_next_run_at(&correct, now), Some(utc_ms(9, 9, 0)));
    }

    #[test]
    fn seeded_next_run_at_with_a_slow_clock() {
        // Daily 9am ran Monday 9:00 and stored Tuesday 9:00; the clock
        // reads Monday 8:30. The valid target must not move back to
        // Monday 9:00, which would run the agent twice.
        let daily = schedule(cron("0 9 * * *"), Some(utc_ms(10, 9, 0)));
        assert_eq!(
            seeded_next_run_at(&daily, utc_ms(9, 8, 30)),
            Some(utc_ms(10, 9, 0))
        );

        // "Weekdays" stored on Sunday by the old numbering is recomputed
        // from the clock even when it reads earlier than the last run.
        let weekdays = schedule(cron("0 9 * * 1-5"), Some(utc_ms(8, 9, 0)));
        assert_eq!(
            seeded_next_run_at(&weekdays, utc_ms(5, 8, 30)),
            Some(utc_ms(5, 9, 0))
        );
    }

    #[test]
    fn seeded_next_run_at_keeps_other_targets() {
        let now = utc_ms(8, 0, 0);
        let past_due = schedule(cron("0 9 * * 1"), Some(utc_ms(7, 9, 0)));
        assert_eq!(seeded_next_run_at(&past_due, now), Some(utc_ms(7, 9, 0)));

        let invalid = schedule(cron("0 9 * * 8"), Some(utc_ms(8, 9, 0)));
        assert_eq!(seeded_next_run_at(&invalid, now), Some(utc_ms(8, 9, 0)));

        let interval = ScheduleKind::Interval {
            interval_minutes: 5,
        };
        let interval = schedule(interval, Some(utc_ms(8, 9, 0)));
        assert_eq!(seeded_next_run_at(&interval, now), Some(utc_ms(8, 9, 0)));

        assert_eq!(
            seeded_next_run_at(&schedule(cron("0 9 * * 1"), None), now),
            None
        );
    }

    #[tokio::test]
    async fn test_create_and_remove_instance_for_agent() {
        let scheduler = create_shared_scheduler();

        let agent = create_test_agent_config();
        {
            let mut s = scheduler.lock().await;
            s.register_definition(AgentDefinition::new(&agent.id, &agent.name));
        }

        create_instance_for_agent(&scheduler, &agent.id).await;

        {
            let s = scheduler.lock().await;
            assert_eq!(s.instance_count(), 1);
            let instance = s.get_instance(&AgentInstance::instance_id_for(&agent.id));
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
            s.register_definition(AgentDefinition::new(&agent.id, &agent.name));
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
