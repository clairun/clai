pub mod ask_user;
pub mod command_splitter;
pub mod history_query;
pub mod local;
pub mod posix_shell;
pub mod prefix_detector;
pub mod registry;
pub mod router;
pub mod vega_chart;
pub mod workspace_tasks;

pub use registry::available_tools;
pub use router::execute_tool;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::assistant::network::run_state::RunNetwork;
use crate::assistant::run_context::RunContext;
use crate::assistant::types::{RunId, SessionId, ToolCallId};
use crate::config::FilesystemPathGrant;

/// Terminal human-wait handling: if a CLAI-owned prompt for user input or
/// permission is abandoned without a decision, cancel the run and never return
/// a tool result for the model to route around. The outer run driver races
/// tool execution against this token and will drop this parked future while
/// cancelling the run.
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

/// Backstop timeout CLAI configures on every CLI provider's MCP *client*.
/// CLAI-owned human waits (`ask_user`, command approval, path grants) do not
/// expire on their own; they block until the user decides or the run is
/// cancelled. Some CLI hosts still require a finite MCP tool timeout, so keep
/// this high enough that unattended prompts behave like a paused terminal
/// session for ordinary away-from-desktop cases while cancellation remains the
/// explicit escape hatch.
///
/// All three CLI providers need this same value; only the transport differs, so
/// each renders it into its own unit at the call site:
/// - Claude Code: `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT` env var (milliseconds)
/// - Codex: `mcp_servers.clai.tool_timeout_sec` config flag (seconds)
/// - OpenCode: `timeout` MCP JSON field (milliseconds)
pub const CLI_MCP_CLIENT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

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
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: Option<ToolCallId>,
    pub cancel_token: CancellationToken,
    pub run_context: Arc<RunContext>,
    /// Set when something the user should know about happened without
    /// failing the run (a denied command or grant, an unavailable sandbox, a
    /// granted path); the run then completes with warnings.
    pub had_warnings: Arc<AtomicBool>,
    /// Run-scoped filesystem grants accepted via `fs_request_grant` modal.
    /// These live alongside the durable grants in `execution.filesystem.
    /// extra_paths` and the credentials-preset paths; they vanish when the
    /// run ends. `AllowOnce` decisions land here only; `AllowAlways` also
    /// lands here so the running tool sees the grant immediately *and* gets
    /// persisted to the agent's DB row so the next session picks it up.
    pub session_grants: Arc<Mutex<Vec<FilesystemPathGrant>>>,
    /// Ask-first rules removed by "Always allow" during this run. The run
    /// reads its settings from a snapshot taken at start, so without this a
    /// rule the user just removed would keep asking until the next run.
    pub run_removed_ask_first: Arc<Mutex<Vec<String>>>,
    /// The agent's host lists parsed once for the run, the hosts allowed
    /// for this run only, and the hosts the run tried to reach.
    pub run_network: Arc<RunNetwork>,
}

/// The run's warning flag as it starts: already set when the agent's
/// network lists hold an entry that was skipped (see
/// [`RunNetwork::has_ignored_entries`]), so that run ends with warnings.
pub fn run_warnings(run_network: &RunNetwork) -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(run_network.has_ignored_entries()))
}

impl ToolExecutionContext {
    pub fn note_warning(&self) {
        self.had_warnings.store(true, Ordering::Relaxed);
    }

    pub fn had_warnings(&self) -> bool {
        self.had_warnings.load(Ordering::Relaxed)
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

    /// Record ask-first rules removed during this run. Idempotent.
    pub fn add_run_removed_ask_first(&self, rules: &[String]) {
        if let Ok(mut removed) = self.run_removed_ask_first.lock() {
            for rule in rules {
                if !removed.contains(rule) {
                    removed.push(rule.clone());
                }
            }
        }
    }

    /// Snapshot of the ask-first rules removed during this run.
    pub fn run_removed_ask_first_snapshot(&self) -> Vec<String> {
        self.run_removed_ask_first
            .lock()
            .map(|rules| rules.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_with_a_skipped_host_entry_starts_with_a_warning() {
        use crate::config::{NetworkMode, NetworkPolicy};
        let run = |allow: &str| {
            run_warnings(&RunNetwork::new(&NetworkPolicy {
                mode: NetworkMode::Allowlist,
                allow_domains: vec![allow.to_string()],
                deny_domains: Vec::new(),
            }))
            .load(Ordering::Relaxed)
        };
        assert!(run("github.com:443"));
        assert!(!run("github.com"));
    }

    #[test]
    fn cli_mcp_client_timeout_is_a_large_idle_backstop() {
        // CLAI-owned human waits have no app-side response timeout. CLI hosts
        // still need a finite MCP timeout value, so keep the shared value large
        // enough for ordinary away-from-desktop waits.
        assert_eq!(CLI_MCP_CLIENT_TIMEOUT, Duration::from_secs(24 * 60 * 60));
    }

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
