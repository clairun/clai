pub mod command_splitter;
pub mod local;
pub mod prefix_detector;
pub mod registry;
pub mod router;
pub mod workspace_tasks;

pub use registry::available_tools;
pub use router::execute_tool;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::assistant::types::{
    RunId, RunNotice, RunNoticeKind, SessionId, ToolCallId, WorkspaceAgentSummary,
};
use crate::config::{ExecutionCapabilityConfig, FilesystemPathGrant};

/// Context for tool execution within an assistant run.
#[allow(dead_code)]
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: Option<ToolCallId>,
    pub workspace_id: Option<String>,
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub mcp_server_ids: Vec<String>,
    pub agent_workspace_id: Option<String>,
    pub workspace_root: Option<PathBuf>,
    pub automation_id: Option<String>,
    pub workspace_agents: Vec<WorkspaceAgentSummary>,
    pub inter_agent_call_depth: Option<u32>,
    pub execution: ExecutionCapabilityConfig,
    pub notices: Arc<Mutex<Vec<RunNotice>>>,
    /// Run-scoped filesystem grants accepted via `fs_request_grant` modal.
    /// These live alongside the durable grants in `execution.filesystem.
    /// extra_paths` and the credentials-preset paths; they vanish when the
    /// run ends. `AllowOnce` decisions land here only; `AllowAlways` also
    /// lands here so the running tool sees the grant immediately *and* gets
    /// persisted to the agent's DB row so the next session picks it up.
    pub session_grants: Arc<Mutex<Vec<FilesystemPathGrant>>>,
}

impl ToolExecutionContext {
    /// Record a policy notice (e.g. command denied) on this run.
    pub fn add_notice(&self, kind: RunNoticeKind, message: String) {
        if let Ok(mut notices) = self.notices.lock() {
            notices.push(RunNotice {
                kind,
                message,
                timestamp: chrono::Utc::now().timestamp_millis(),
            });
        }
    }

    /// Drain all accumulated notices.
    pub fn take_notices(&self) -> Vec<RunNotice> {
        self.notices
            .lock()
            .map(|mut n| std::mem::take(&mut *n))
            .unwrap_or_default()
    }

    /// Append a run-scoped grant. Idempotent on (path, access) — duplicate
    /// adds are dropped so an LLM that requests the same path twice doesn't
    /// inflate the bind list.
    pub fn add_session_grant(&self, grant: FilesystemPathGrant) {
        if let Ok(mut grants) = self.session_grants.lock() {
            if !grants
                .iter()
                .any(|g| g.path == grant.path && g.access == grant.access)
            {
                grants.push(grant);
            }
        }
    }

    /// Snapshot of run-scoped grants for merging with durable grants when
    /// building the effective grant set.
    pub fn session_grants_snapshot(&self) -> Vec<FilesystemPathGrant> {
        self.session_grants
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}
