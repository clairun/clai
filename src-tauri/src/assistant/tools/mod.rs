pub mod ask_user;
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
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::assistant::types::{
    RunId, RunNotice, RunNoticeKind, SessionId, ToolCallId, WorkspaceAgentSummary,
};
use crate::config::{ExecutionCapabilityConfig, FilesystemPathGrant};

/// Terminal human-wait handling: once a CLAI-owned prompt for user input or
/// permission expires, cancel the run and never return a tool result for the
/// model to route around. The outer run driver races tool execution against
/// this token and will drop this parked future while cancelling the run.
pub async fn cancel_run_and_park<T>(cancel_token: &CancellationToken) -> T {
    cancel_token.cancel();
    std::future::pending::<T>().await
}

/// The name under which clai's local MCP server is registered with CLI
/// providers (`write_mcp_config` for Claude Code, `add_codex_common_args`
/// for Codex — both in `assistant::local_agent`).
pub const LOCAL_MCP_SERVER_NAME: &str = "clai";

/// The `mcp__<server>__` qualifier Claude Code prepends to tools it
/// discovered on our local MCP server (`mcp__clai__web_fetch`).
pub const LOCAL_MCP_TOOL_PREFIX: &str = "mcp__clai__";

/// CLI providers set their local-MCP client timeout to 60 minutes. Human
/// waits time out just before that so CLAI owns cleanup/cancellation instead
/// of letting the CLI report an ordinary tool timeout back to the model.
pub const CLI_INTERACTIVE_WAIT_TIMEOUT: Duration = Duration::from_secs(55 * 60);

/// Strips the CLI-side qualifier from a tool name that was recorded under
/// (or mimicked from) a Claude Code run: `mcp__clai__web_fetch` →
/// `web_fetch`. External MCP tools proxied through the local server lose
/// only the outer qualifier (`mcp__clai__mcp__1c47ed2d__x` →
/// `mcp__1c47ed2d__x`, which is their canonical internal name). Names
/// without the qualifier pass through unchanged — external server short
/// ids are 8 hex chars, so `mcp__clai__` can never denote one of them.
pub fn strip_local_mcp_qualifier(name: &str) -> &str {
    name.strip_prefix(LOCAL_MCP_TOOL_PREFIX).unwrap_or(name)
}

/// Context for tool execution within an assistant run.
#[allow(dead_code)]
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: Option<ToolCallId>,
    pub cancel_token: CancellationToken,
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
    /// Run-scoped allowed command prefixes accepted via the bash approval
    /// modal. Mirrors `session_grants` for commands: both `AllowOnce` and
    /// `AllowAlways` populate this so the same command (or a descendant
    /// of the chosen prefix) won't re-prompt the user for the rest of
    /// the run. `AllowAlways` is additionally persisted to the agent's
    /// durable `allowed_command_prefixes`; `AllowOnce` is run-scoped only
    /// and vanishes when the run ends. Without this cache, every bash
    /// invocation re-evaluated only the durable list (which mid-run
    /// reflects neither the just-accepted `AllowOnce` nor the
    /// just-accepted `AllowAlways`, since persistence updates the DB but
    /// not the running execution snapshot) and re-prompted every time.
    pub session_allowed_command_prefixes: Arc<Mutex<Vec<String>>>,
    /// Run-scoped blocked command prefixes — symmetric counterpart of
    /// `session_allowed_command_prefixes` for `DenyAlways` decisions. A
    /// `DenyAlways` mid-run otherwise persists to the agent's durable
    /// `blocked_command_prefixes` but isn't visible to the running
    /// `context.execution`, so the LLM's next retry of the same command
    /// would re-prompt the user. `DenyOnce` is deliberately NOT cached:
    /// it's a one-shot decision by design, and re-prompting on retry
    /// lets the user reconsider.
    pub session_blocked_command_prefixes: Arc<Mutex<Vec<String>>>,
}

impl ToolExecutionContext {
    pub fn interactive_wait_timeout(&self, default: Duration) -> Duration {
        if self.tool_call_id.is_none() {
            CLI_INTERACTIVE_WAIT_TIMEOUT
        } else {
            default
        }
    }

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

    /// Append a run-scoped allowed command prefix. Idempotent on string
    /// equality so re-approving the same command doesn't bloat the list.
    pub fn add_session_allowed_command_prefix(&self, prefix: String) {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(mut prefixes) = self.session_allowed_command_prefixes.lock() {
            if !prefixes.iter().any(|p| p == trimmed) {
                prefixes.push(trimmed.to_string());
            }
        }
    }

    /// Snapshot of run-scoped allowed command prefixes for merging with
    /// the durable allowlist in policy evaluation.
    pub fn session_allowed_command_prefixes_snapshot(&self) -> Vec<String> {
        self.session_allowed_command_prefixes
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    /// Append a run-scoped blocked command prefix. Idempotent on string
    /// equality. Mirrors [`Self::add_session_allowed_command_prefix`].
    pub fn add_session_blocked_command_prefix(&self, prefix: String) {
        let trimmed = prefix.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(mut prefixes) = self.session_blocked_command_prefixes.lock() {
            if !prefixes.iter().any(|p| p == trimmed) {
                prefixes.push(trimmed.to_string());
            }
        }
    }

    /// Snapshot of run-scoped blocked command prefixes for merging with
    /// the durable blocklist in policy evaluation.
    pub fn session_blocked_command_prefixes_snapshot(&self) -> Vec<String> {
        self.session_blocked_command_prefixes
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_constant_matches_server_name() {
        assert_eq!(
            LOCAL_MCP_TOOL_PREFIX,
            format!("mcp__{}__", LOCAL_MCP_SERVER_NAME)
        );
    }

    #[test]
    fn strips_local_mcp_qualifier_from_builtins() {
        assert_eq!(
            strip_local_mcp_qualifier("mcp__clai__web_fetch"),
            "web_fetch"
        );
        assert_eq!(
            strip_local_mcp_qualifier("mcp__clai__bash_exec"),
            "bash_exec"
        );
        assert_eq!(strip_local_mcp_qualifier("mcp__clai__ask_user"), "ask_user");
    }

    #[test]
    fn strips_only_the_outer_qualifier_for_proxied_external_tools() {
        assert_eq!(
            strip_local_mcp_qualifier("mcp__clai__mcp__1c47ed2d__search"),
            "mcp__1c47ed2d__search"
        );
    }

    #[test]
    fn leaves_unqualified_and_external_names_unchanged() {
        assert_eq!(strip_local_mcp_qualifier("web_fetch"), "web_fetch");
        assert_eq!(
            strip_local_mcp_qualifier("mcp__1c47ed2d__search"),
            "mcp__1c47ed2d__search"
        );
        assert_eq!(strip_local_mcp_qualifier("mcp__clai"), "mcp__clai");
    }
}
