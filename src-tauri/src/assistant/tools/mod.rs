pub mod local;
pub mod registry;
pub mod router;
pub mod workspace_tasks;

pub use registry::available_tools;
pub use router::execute_tool;

use std::sync::{Arc, Mutex};

use crate::assistant::types::{
    RunId, RunNotice, RunNoticeKind, SessionId, ToolCallId, WorkspaceAgentSummary,
};
use crate::config::ExecutionCapabilityConfig;

/// Context for tool execution within an assistant run.
#[allow(dead_code)]
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: Option<ToolCallId>,
    pub tab_id: Option<String>,
    pub workspace_id: Option<String>,
    pub space_id: Option<String>,
    pub room_id: Option<String>,
    pub mcp_server_ids: Vec<String>,
    pub agent_workspace_id: Option<String>,
    pub automation_id: Option<String>,
    pub workspace_agents: Vec<WorkspaceAgentSummary>,
    pub inter_agent_call_depth: Option<u32>,
    pub execution: ExecutionCapabilityConfig,
    pub notices: Arc<Mutex<Vec<RunNotice>>>,
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
}
