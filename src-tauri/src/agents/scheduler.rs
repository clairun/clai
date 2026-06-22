//! Agent scheduler.
//!
//! A simple scheduler that manages agent instances and determines
//! when they should run. Only one agent runs at a time.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                         SCHEDULER                               │
//! │                                                                 │
//! │  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐     │
//! │  │  Definitions │    │  Instances   │    │  next_ready  │     │
//! │  │  (templates) │───▶│ (per space)  │───▶│  (find work) │     │
//! │  └──────────────┘    └──────────────┘    └──────────────┘     │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Mutex;

use super::types::{AgentDefinition, AgentInstance};

// =============================================================================
// Scheduler State
// =============================================================================

/// Current state of the scheduler.
#[derive(Debug, Clone, PartialEq)]
pub enum SchedulerState {
    /// Scheduler is running normally.
    Running,
    /// Scheduler is paused (e.g., no connectivity).
    Paused { reason: String },
    /// Scheduler is stopped.
    Stopped,
}

// =============================================================================
// Scheduler
// =============================================================================

/// The agent scheduler.
///
/// Manages agent definitions and instances. Finds the next agent
/// that is ready to run. Only one agent can run at a time.
pub struct Scheduler {
    /// Current scheduler state.
    state: SchedulerState,

    /// Agent definitions (templates).
    definitions: HashMap<String, AgentDefinition>,

    /// Agent instances (per space/room).
    instances: HashMap<String, AgentInstance>,

    /// Currently running instance ID (only one at a time).
    running: Option<String>,
}

impl Scheduler {
    /// Creates a new scheduler.
    pub fn new() -> Self {
        Self {
            state: SchedulerState::Running,
            definitions: HashMap::new(),
            instances: HashMap::new(),
            running: None,
        }
    }

    // =========================================================================
    // Registration
    // =========================================================================

    /// Registers an agent definition.
    pub fn register_definition(&mut self, definition: AgentDefinition) {
        self.definitions.insert(definition.id.clone(), definition);
    }

    /// Gets an agent definition.
    pub fn get_definition(&self, agent_id: &str) -> Option<&AgentDefinition> {
        self.definitions.get(agent_id)
    }

    /// Removes an agent definition. Pair with [`remove_instances_for_agent`]
    /// when the underlying agent is being deleted — clearing the definition
    /// keeps the in-memory map from accumulating stale entries.
    pub fn remove_definition(&mut self, agent_id: &str) -> Option<AgentDefinition> {
        self.definitions.remove(agent_id)
    }

    /// Creates and registers an agent instance for a space/room.
    ///
    /// Returns `None` if:
    /// - The agent definition doesn't exist
    /// - An instance already exists for this agent/space/room combination
    pub fn create_instance(
        &mut self,
        agent_id: &str,
        space_id: impl Into<String>,
        room_id: impl Into<String>,
    ) -> Option<String> {
        let definition = self.definitions.get(agent_id)?;
        let instance = AgentInstance::new(definition, space_id.into(), room_id.into());
        let instance_id = instance.instance_id.clone();

        // Don't overwrite existing instances
        if self.instances.contains_key(&instance_id) {
            return Some(instance_id);
        }

        self.instances.insert(instance_id.clone(), instance);

        Some(instance_id)
    }

    /// Removes an agent instance.
    pub fn remove_instance(&mut self, instance_id: &str) {
        self.instances.remove(instance_id);
        if self.running.as_ref() == Some(&instance_id.to_string()) {
            self.running = None;
        }
    }

    /// Removes all instances for a given agent ID.
    ///
    /// Returns the number of instances removed.
    pub fn remove_instances_for_agent(&mut self, agent_id: &str) -> usize {
        let to_remove: Vec<String> = self
            .instances
            .keys()
            .filter(|id| id.starts_with(&format!("{}:", agent_id)))
            .cloned()
            .collect();

        let count = to_remove.len();
        for id in to_remove {
            self.remove_instance(&id);
        }
        count
    }

    // =========================================================================
    // Scheduling
    // =========================================================================

    /// Gets the next agent that is ready to run.
    ///
    /// Returns `None` if:
    /// - Scheduler is paused/stopped
    /// - An agent is already running
    /// - No agents are ready
    pub fn next_ready(&mut self) -> Option<String> {
        // Check scheduler state
        if self.state != SchedulerState::Running {
            return None;
        }

        // Only one agent at a time
        if self.running.is_some() {
            return None;
        }

        let now = Instant::now();

        // Find first ready instance
        for instance in self.instances.values_mut() {
            if instance.is_ready(now) {
                instance.is_running = true;
                self.running = Some(instance.instance_id.clone());
                return Some(instance.instance_id.clone());
            }
        }

        None
    }

    /// Marks an agent as completed and schedules its next run.
    ///
    /// # Arguments
    ///
    /// * `instance_id` - The instance to mark complete
    /// * `success` - Whether the run was successful
    /// * `next_run_at_unix_ms` - Wall-clock target for the next fire,
    ///   computed by `agents::schedule::compute_next_run_at` against the
    ///   workspace's current `ScheduleKind`. The scheduler translates
    ///   this into an `Instant` via `set_instance_next_run_at`.
    pub fn complete_agent(&mut self, instance_id: &str, success: bool, next_run_at_unix_ms: i64) {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.is_running = false;
            // Clear the one-shot manual-run flag: a paused instance that
            // ran via `force_ready` should drop back to paused, not keep
            // ticking.
            instance.manual_run_pending = false;
            if !success {
                // Could track consecutive failures here if needed
            }
        }

        // Seed the in-memory next_run_at from the wall-clock target so
        // both interval and cron modes share one code path. Past
        // targets clear next_run_at to "ready-now", matching the
        // catch-up semantic.
        self.set_instance_next_run_at(instance_id, Some(next_run_at_unix_ms));

        if self.running.as_ref() == Some(&instance_id.to_string()) {
            self.running = None;
        }
    }

    /// Releases a currently-running instance back to *ready-now* without
    /// advancing its schedule. Use this when a due tick could not actually
    /// run — e.g. the workspace is busy with a user-driven (interactive)
    /// run — so the next tick re-evaluates and fires as soon as the
    /// workspace is idle, instead of silently skipping this scheduled fire
    /// until the next interval.
    ///
    /// Differs from [`complete_agent`] in two ways: it does **not** compute
    /// or persist a next-run anchor (the fire is deferred, not consumed),
    /// and it does **not** clear `manual_run_pending` (a deferred "Run now"
    /// must still fire once the workspace frees up).
    pub fn defer_running_instance(&mut self, instance_id: &str) {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.is_running = false;
            // Ready-now: the next runner tick retries; `is_ready` still
            // gates on `enabled || manual_run_pending`, so a paused
            // schedule won't start ticking on its own.
            instance.next_run_at = None;
        }
        if self.running.as_ref() == Some(&instance_id.to_string()) {
            self.running = None;
        }
    }

    // =========================================================================
    // State Management
    // =========================================================================

    /// Pauses the scheduler.
    pub fn pause(&mut self, reason: String) {
        self.state = SchedulerState::Paused { reason };
    }

    /// Resumes the scheduler.
    pub fn resume(&mut self) {
        self.state = SchedulerState::Running;
    }

    /// Stops the scheduler.
    pub fn stop(&mut self) {
        self.state = SchedulerState::Stopped;
    }

    /// Gets the current scheduler state.
    pub fn state(&self) -> &SchedulerState {
        &self.state
    }

    /// Returns whether an agent is currently running.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Gets the currently running instance ID.
    pub fn running_instance(&self) -> Option<&String> {
        self.running.as_ref()
    }

    // =========================================================================
    // Instance Access
    // =========================================================================

    /// Gets a reference to an agent instance.
    pub fn get_instance(&self, instance_id: &str) -> Option<&AgentInstance> {
        self.instances.get(instance_id)
    }

    /// Gets a mutable reference to an agent instance.
    pub fn get_instance_mut(&mut self, instance_id: &str) -> Option<&mut AgentInstance> {
        self.instances.get_mut(instance_id)
    }

    /// Forces an agent to be ready for immediate execution by clearing its
    /// next_run_at. The runner loop will pick it up on its next tick.
    /// Returns true if the instance was found and updated.
    ///
    /// Works on disabled (paused) instances too — sets `manual_run_pending`
    /// so the next `is_ready` check passes once. Cleared by
    /// `complete_agent` so a paused schedule resumes its pause after the
    /// one-shot manual run, instead of going back to ticking on its own.
    pub fn force_ready(&mut self, agent_id: &str) -> bool {
        for instance in self.instances.values_mut() {
            if instance.agent_id == agent_id && !instance.is_running {
                instance.next_run_at = None;
                instance.manual_run_pending = true;
                return true;
            }
        }
        false
    }

    /// Gets all instances for a space.
    pub fn get_instances_for_space(&self, space_id: &str) -> Vec<&AgentInstance> {
        self.instances
            .values()
            .filter(|i| i.space_id == space_id)
            .collect()
    }

    /// Gets the count of all registered instances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Gets all instances.
    pub fn all_instances(&self) -> impl Iterator<Item = &AgentInstance> {
        self.instances.values()
    }

    /// Enables or disables an agent instance.
    pub fn set_instance_enabled(&mut self, instance_id: &str, enabled: bool) {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.enabled = enabled;
        }
    }

    /// Seeds the instance's in-memory `next_run_at` from a persisted
    /// wall-clock target. Used at startup (and on schedule reconcile) so
    /// a previously-scheduled workspace continues from where it left off
    /// instead of firing immediately on app launch.
    ///
    /// `target_unix_ms = None` clears `next_run_at` (instance becomes
    /// ready-now, matching the "fresh schedule, no anchor" semantic).
    /// A past target also clears `next_run_at` so the instance catches
    /// up on the next tick — useful when the app was down across one or
    /// more intervals.
    pub fn set_instance_next_run_at(&mut self, instance_id: &str, target_unix_ms: Option<i64>) {
        let Some(instance) = self.instances.get_mut(instance_id) else {
            return;
        };
        let Some(target) = target_unix_ms else {
            instance.next_run_at = None;
            return;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if target <= now_ms {
            // Past target — let the next tick pick it up.
            instance.next_run_at = None;
            return;
        }
        let remaining = (target - now_ms) as u64;
        instance.next_run_at = Some(Instant::now() + std::time::Duration::from_millis(remaining));
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe scheduler wrapped in Arc<Mutex>.
pub type SharedScheduler = Arc<Mutex<Scheduler>>;

/// Creates a new shared scheduler.
pub fn create_shared_scheduler() -> SharedScheduler {
    Arc::new(Mutex::new(Scheduler::new()))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_definition() -> AgentDefinition {
        AgentDefinition::new("test-agent", "Test Agent").with_description("A test agent")
    }

    #[test]
    fn test_scheduler_creation() {
        let scheduler = Scheduler::new();

        assert_eq!(scheduler.state(), &SchedulerState::Running);
        assert!(!scheduler.is_running());
        assert_eq!(scheduler.instance_count(), 0);
    }

    #[test]
    fn test_register_and_create_instance() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());

        let instance_id = scheduler
            .create_instance("test-agent", "space1", "room1")
            .unwrap();

        assert_eq!(instance_id, "test-agent:space1:room1");
        assert_eq!(scheduler.instance_count(), 1);

        let instance = scheduler.get_instance(&instance_id).unwrap();
        assert_eq!(instance.space_id, "space1");
        assert_eq!(instance.room_id, "room1");
    }

    #[test]
    fn test_scheduler_pause_resume() {
        let mut scheduler = Scheduler::new();

        assert_eq!(scheduler.state(), &SchedulerState::Running);

        scheduler.pause("Testing".to_string());
        assert!(matches!(scheduler.state(), SchedulerState::Paused { .. }));

        scheduler.resume();
        assert_eq!(scheduler.state(), &SchedulerState::Running);
    }

    #[test]
    fn test_next_ready_basic() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());
        scheduler.create_instance("test-agent", "space1", "room1");

        // First call should return the instance
        let next = scheduler.next_ready();
        assert_eq!(next, Some("test-agent:space1:room1".to_string()));

        // Second call should return None (agent is running)
        let next = scheduler.next_ready();
        assert!(next.is_none());

        // After completion, should be scheduled for later (not immediately ready)
        let target = chrono::Utc::now().timestamp_millis() + 60_000;
        scheduler.complete_agent("test-agent:space1:room1", true, target);
        let next = scheduler.next_ready();
        assert!(next.is_none()); // Scheduled for 60 seconds later
    }

    #[test]
    fn test_only_one_agent_at_a_time() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());
        scheduler.create_instance("test-agent", "space1", "room1");
        scheduler.create_instance("test-agent", "space2", "room1");

        // First should succeed
        let first = scheduler.next_ready();
        assert!(first.is_some());

        // Second should fail (only one at a time)
        let second = scheduler.next_ready();
        assert!(second.is_none());

        // Complete first
        let target = chrono::Utc::now().timestamp_millis() + 60_000;
        scheduler.complete_agent(&first.unwrap(), true, target);

        // Now second should work (but it's scheduled for later)
        // Both instances were scheduled, so they'll run based on next_run_at
        assert!(!scheduler.is_running());
    }

    #[test]
    fn test_paused_scheduler_returns_none() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());
        scheduler.create_instance("test-agent", "space1", "room1");

        scheduler.pause("Maintenance".to_string());

        let next = scheduler.next_ready();
        assert!(next.is_none());

        scheduler.resume();

        let next = scheduler.next_ready();
        assert!(next.is_some());
    }

    #[test]
    fn test_remove_instance() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());
        let instance_id = scheduler
            .create_instance("test-agent", "space1", "room1")
            .unwrap();

        assert_eq!(scheduler.instance_count(), 1);

        scheduler.remove_instance(&instance_id);
        assert_eq!(scheduler.instance_count(), 0);
    }

    #[test]
    fn test_enable_disable_instance() {
        let mut scheduler = Scheduler::new();

        scheduler.register_definition(create_test_definition());
        scheduler.create_instance("test-agent", "space1", "room1");

        // Disable the instance
        scheduler.set_instance_enabled("test-agent:space1:room1", false);

        // Should not be ready
        let next = scheduler.next_ready();
        assert!(next.is_none());

        // Re-enable
        scheduler.set_instance_enabled("test-agent:space1:room1", true);

        // Should be ready now
        let next = scheduler.next_ready();
        assert!(next.is_some());
    }

    // -------------------------------------------------------------------
    // set_instance_next_run_at: restart-survival seed
    // -------------------------------------------------------------------

    #[test]
    fn set_instance_next_run_at_future_target_defers_readiness() {
        let mut scheduler = Scheduler::new();
        scheduler.register_definition(create_test_definition());
        let instance_id = scheduler.create_instance("test-agent", "", "").unwrap();

        // Target 1 hour in the future — instance should not be ready.
        let future = chrono::Utc::now().timestamp_millis() + 60 * 60 * 1000;
        scheduler.set_instance_next_run_at(&instance_id, Some(future));

        assert!(scheduler.next_ready().is_none());
    }

    #[test]
    fn set_instance_next_run_at_past_target_clears_to_ready_now() {
        let mut scheduler = Scheduler::new();
        scheduler.register_definition(create_test_definition());
        let instance_id = scheduler.create_instance("test-agent", "", "").unwrap();

        // Mark instance not-ready first by giving it a far-future anchor.
        let future = chrono::Utc::now().timestamp_millis() + 60 * 60 * 1000;
        scheduler.set_instance_next_run_at(&instance_id, Some(future));
        assert!(scheduler.next_ready().is_none());

        // Past anchor (1 hour ago) — clears next_run_at so the next tick
        // picks it up. Matches "catch up after restart" semantics when
        // the app was down across an interval boundary.
        let past = chrono::Utc::now().timestamp_millis() - 60 * 60 * 1000;
        scheduler.set_instance_next_run_at(&instance_id, Some(past));
        assert!(scheduler.next_ready().is_some());
    }

    #[test]
    fn set_instance_next_run_at_none_clears_to_ready_now() {
        let mut scheduler = Scheduler::new();
        scheduler.register_definition(create_test_definition());
        let instance_id = scheduler.create_instance("test-agent", "", "").unwrap();

        // Park far in the future, then clear with None.
        let future = chrono::Utc::now().timestamp_millis() + 60 * 60 * 1000;
        scheduler.set_instance_next_run_at(&instance_id, Some(future));
        scheduler.set_instance_next_run_at(&instance_id, None);

        assert!(scheduler.next_ready().is_some());
    }

    // -------------------------------------------------------------------
    // defer_running_instance: release a due-but-blocked tick without
    // consuming the schedule (busy-workspace deferral)
    // -------------------------------------------------------------------

    #[test]
    fn defer_running_instance_releases_slot_and_stays_ready_now() {
        let mut scheduler = Scheduler::new();
        scheduler.register_definition(create_test_definition());
        let id = scheduler
            .create_instance("test-agent", "space1", "room1")
            .unwrap();

        // Claim the instance (simulates a due tick).
        assert_eq!(scheduler.next_ready(), Some(id.clone()));
        assert!(scheduler.is_running());

        // Workspace was busy: defer instead of completing.
        scheduler.defer_running_instance(&id);

        // Slot is released and the instance is immediately ready again,
        // so the next tick retries (no schedule consumed).
        assert!(!scheduler.is_running());
        assert_eq!(scheduler.next_ready(), Some(id));
    }

    #[test]
    fn defer_running_instance_preserves_manual_run_pending() {
        let mut scheduler = Scheduler::new();
        scheduler.register_definition(create_test_definition());
        let id = scheduler
            .create_instance("test-agent", "space1", "room1")
            .unwrap();

        // Pause the schedule, then queue a one-shot manual run.
        scheduler.set_instance_enabled(&id, false);
        assert!(scheduler.force_ready("test-agent"));
        assert!(scheduler.get_instance(&id).unwrap().manual_run_pending);

        // Manual run is picked up despite being paused.
        assert_eq!(scheduler.next_ready(), Some(id.clone()));

        // Deferred because the workspace was busy: the manual-run flag must
        // survive so the run still fires once idle (complete_agent would
        // have cleared it).
        scheduler.defer_running_instance(&id);
        assert!(scheduler.get_instance(&id).unwrap().manual_run_pending);
        assert_eq!(scheduler.next_ready(), Some(id));
    }

    #[test]
    fn defer_running_instance_ignores_unknown_instance() {
        let mut scheduler = Scheduler::new();
        // Must not panic on a missing id (covers the early-return branch).
        scheduler.defer_running_instance("nonexistent");
        assert!(!scheduler.is_running());
    }

    #[test]
    fn set_instance_next_run_at_ignores_unknown_instance() {
        let mut scheduler = Scheduler::new();
        // Just a smoke check that the function doesn't panic on a
        // missing id — covers the early-return branch.
        scheduler.set_instance_next_run_at("nonexistent", Some(0));
    }
}
