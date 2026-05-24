//! Agent type definitions.
//!
//! These are the runtime types used by the scheduler and executor.

use serde::{Deserialize, Serialize};
use std::time::Instant;

// =============================================================================
// Agent Definition
// =============================================================================

/// Runtime definition of an agent.
///
/// This is the processed/compiled version of an AgentConfig from the config file.
/// It is used by the scheduler and executor.
///
/// # Fields
///
/// - `id`: Unique identifier (UUID from config)
/// - `name`: Human-readable name for UI
/// - `description`: Description of what this agent does
/// - `interval_ms`: How often to run (in milliseconds)
/// - `required_tools`: Tool namespaces this agent needs (e.g., ["netdata", "canvas"])
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique identifier for this agent type.
    pub id: String,

    /// Human-readable name.
    pub name: String,

    /// Description of what this agent does.
    #[serde(default)]
    pub description: String,

    /// How often to run this agent (in milliseconds).
    pub interval_ms: u64,

    /// List of tool namespaces this agent needs (e.g., ["netdata", "canvas", "tabs"]).
    ///
    /// The executor will only expose tools from these namespaces to the AI.
    /// Available namespaces: "netdata", "canvas", "tabs"
    #[serde(default)]
    pub required_tools: Vec<String>,
}

impl AgentDefinition {
    /// Creates a new agent definition.
    pub fn new(id: &str, name: &str, interval_ms: u64) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            interval_ms,
            required_tools: vec![],
        }
    }

    /// Sets the description.
    pub fn with_description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self
    }

    /// Sets the required tools.
    pub fn with_tools(mut self, tools: Vec<&str>) -> Self {
        self.required_tools = tools.into_iter().map(String::from).collect();
        self
    }
}

// =============================================================================
// Agent Instance
// =============================================================================

/// A running instance of an agent for a specific space/room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstance {
    /// Reference to the agent definition ID.
    pub agent_id: String,

    /// Unique instance ID (e.g., "agent-uuid:space123:room456").
    pub instance_id: String,

    /// Space this agent is monitoring.
    pub space_id: String,

    /// Room this agent is monitoring.
    pub room_id: String,

    /// Whether the agent is currently running.
    #[serde(default)]
    pub is_running: bool,

    /// Whether this instance is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Marks a one-shot manual run requested via `force_ready`. Lets the
    /// runner pick up a *paused* instance for a single tick — the
    /// pause-vs-manual-run distinction the UI relies on. Cleared by
    /// `complete_agent` so the next tick falls back to the regular
    /// `enabled` gate. Not persisted: transient by nature.
    #[serde(skip)]
    pub manual_run_pending: bool,

    /// Conversation ID from the last run (for viewing history).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_conversation_id: Option<String>,

    /// When this agent should run next (not serialized).
    #[serde(skip)]
    pub next_run_at: Option<Instant>,
}

fn default_true() -> bool {
    true
}

impl AgentInstance {
    /// Creates a new agent instance.
    pub fn new(definition: &AgentDefinition, space_id: String, room_id: String) -> Self {
        let instance_id = format!("{}:{}:{}", definition.id, space_id, room_id);

        Self {
            agent_id: definition.id.clone(),
            instance_id,
            space_id,
            room_id,
            is_running: false,
            enabled: true,
            manual_run_pending: false,
            last_conversation_id: None,
            next_run_at: None,
        }
    }

    /// Returns true if this agent is ready to run. A paused instance
    /// (`!enabled`) is still picked up once if a manual run was queued
    /// via `force_ready` — that's how the Fleet "Run now" button works
    /// against paused schedules.
    pub fn is_ready(&self, now: Instant) -> bool {
        (self.enabled || self.manual_run_pending)
            && !self.is_running
            && self.next_run_at.map(|t| t <= now).unwrap_or(true)
    }

    /// Schedules the next run based on the interval.
    pub fn schedule_next(&mut self, interval_ms: u64) {
        self.next_run_at = Some(Instant::now() + std::time::Duration::from_millis(interval_ms));
    }

    /// Returns seconds until the next scheduled run (0 if ready now or no schedule).
    pub fn seconds_until_next_run(&self) -> u64 {
        match self.next_run_at {
            Some(next) => {
                let now = Instant::now();
                if next > now {
                    next.duration_since(now).as_secs()
                } else {
                    0
                }
            }
            None => 0,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_definition_creation() {
        let def = AgentDefinition::new("test-agent", "Test Agent", 60_000)
            .with_description("A test agent")
            .with_tools(vec!["canvas", "tabs"]);

        assert_eq!(def.id, "test-agent");
        assert_eq!(def.name, "Test Agent");
        assert_eq!(def.interval_ms, 60_000);
        assert_eq!(def.required_tools, vec!["canvas", "tabs"]);
    }

    #[test]
    fn test_agent_instance_creation() {
        let def = AgentDefinition::new("test-agent", "Test Agent", 60_000);
        let instance = AgentInstance::new(&def, "space1".to_string(), "room1".to_string());

        assert_eq!(instance.agent_id, "test-agent");
        assert_eq!(instance.instance_id, "test-agent:space1:room1");
        assert_eq!(instance.space_id, "space1");
        assert_eq!(instance.room_id, "room1");
        assert!(!instance.is_running);
        assert!(instance.enabled);
    }

    #[test]
    fn test_agent_instance_is_ready() {
        let def = AgentDefinition::new("test-agent", "Test Agent", 60_000);
        let mut instance = AgentInstance::new(&def, "space1".to_string(), "room1".to_string());
        let now = Instant::now();

        // Initially ready (no next_run_at set)
        assert!(instance.is_ready(now));

        // Not ready when running
        instance.is_running = true;
        assert!(!instance.is_ready(now));
        instance.is_running = false;

        // Not ready when disabled
        instance.enabled = false;
        assert!(!instance.is_ready(now));
        instance.enabled = true;

        // Not ready when scheduled for future
        instance.next_run_at = Some(now + std::time::Duration::from_secs(60));
        assert!(!instance.is_ready(now));

        // Ready when scheduled time has passed
        instance.next_run_at = Some(now - std::time::Duration::from_secs(1));
        assert!(instance.is_ready(now));
    }
}
