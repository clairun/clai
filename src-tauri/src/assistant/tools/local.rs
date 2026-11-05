use crate::assistant::sandbox::filesystem::{
    agent_path_string, resolve_existing, EffectiveFilesystemPolicy,
};
use futures::StreamExt;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::time::Duration;

use crate::assistant::network::addr::{
    is_blocked_web_fetch_hostname, reject_private_web_fetch_addrs, reject_private_web_fetch_ip,
    strip_ipv6_url_brackets,
};
use crate::assistant::network::matcher::{HostDecision, HostLists};
use crate::assistant::sandbox::{
    run_command, SandboxCommand, SandboxEnv, SandboxNetworkMode, SandboxProfile,
    SandboxSessionBusMode,
};
use crate::config::{
    ExecutionCapabilityConfig, FilesystemPathAccess, FilesystemPathGrant, GrantOrigin, NetworkMode,
    NetworkPolicy, SandboxSessionBusConfig, ShellAccessMode,
};

use super::ToolExecutionContext;
use crate::commands::permissions::{AskFirstDecision, AskFirstRow};

const DEFAULT_BASH_TIMEOUT_MS: u64 = 300_000;
const MAX_BASH_TIMEOUT_MS: u64 = 1_800_000;
const DEFAULT_BASH_OUTPUT_LIMIT: usize = 20_000;
const MAX_BASH_OUTPUT_LIMIT: usize = 200_000;
const DEFAULT_WEB_FETCH_CONTENT_LIMIT: usize = 20_000;
const MAX_WEB_FETCH_CONTENT_LIMIT: usize = 100_000;
const MAX_WEB_FETCH_BODY_BYTES: usize = MAX_WEB_FETCH_CONTENT_LIMIT * 4;
const DEFAULT_WEB_FETCH_TIMEOUT_MS: u64 = 15_000;
const MAX_WEB_FETCH_TIMEOUT_MS: u64 = 30_000;
const MAX_WEB_FETCH_REDIRECTS: usize = 5;
const DEFAULT_WEB_SEARCH_MAX_RESULTS: usize = 10;
const MAX_WEB_SEARCH_MAX_RESULTS: usize = 20;
const WEB_SEARCH_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BashExecParams {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_output_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsRequestGrantParams {
    /// Path the agent wants access to. Absolute or `~`-prefixed; the
    /// backend resolves `~` against the host `HOME` env var.
    path: String,
    /// `read_only` or `read_write`.
    access: FilesystemPathAccess,
    /// Why the agent needs this access. Shown to the user in the modal so
    /// they can make an informed decision.
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebSearchParams {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebFetchParams {
    url: String,
    #[serde(default)]
    max_content_chars: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

pub async fn execute_local_tool(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    tool_name: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tool_name {
        "bash_exec" => {
            let params: BashExecParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid bash_exec params: {}", e))?;
            execute_bash_exec(deps, context, params).await
        }
        "fs_request_grant" => {
            let params: FsRequestGrantParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid fs_request_grant params: {}", e))?;
            execute_fs_request_grant(deps, context, params).await
        }
        "web_search" => {
            let params: WebSearchParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid web_search params: {}", e))?;
            execute_web_search(web_gate(context)?, params).await
        }
        "web_fetch" => {
            let params: WebFetchParams = serde_json::from_value(params)
                .map_err(|e| format!("Invalid web_fetch params: {}", e))?;
            execute_web_fetch(web_gate(context)?, params).await
        }
        _ => Err(format!("Unknown local tool: {}", tool_name)),
    }
}

async fn execute_bash_exec(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: BashExecParams,
) -> Result<serde_json::Value, String> {
    if matches!(
        context.run_context.execution.shell.mode,
        ShellAccessMode::Off
    ) {
        return Err("Shell access is disabled for this agent".to_string());
    }

    let filesystem = filesystem_policy(context)?;
    let cwd = filesystem.authorize_cwd(params.cwd.as_deref().unwrap_or("."))?;
    let workspace_root = ensure_workspace_root(context)?;
    // Before any ask-first card: a profile the sandbox cannot enforce fails
    // here, so the user is never asked about a command that cannot run.
    let profile = sandbox_profile(
        context,
        workspace_root,
        filesystem,
        &crate::assistant::sandbox::status(),
    )?;

    if params.command.trim().is_empty() {
        return Err("Shell command cannot be empty".to_string());
    }
    let run_removed = context.run_removed_ask_first_snapshot();
    if let PolicyResult::AskFirst(rows) = evaluate_command_policy(
        &context.run_context.execution,
        &params.command,
        &run_removed,
    ) {
        await_ask_first(deps, context, &params.command, rows).await?;
    }

    // Whether the model set timeoutMs itself — drives the recovery hint on a
    // timeout ("the requested timeout" vs "the default timeout").
    let explicit_timeout = params.timeout_ms.is_some();
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_BASH_TIMEOUT_MS)
        .min(MAX_BASH_TIMEOUT_MS);
    let output_limit = params
        .max_output_chars
        .unwrap_or(DEFAULT_BASH_OUTPUT_LIMIT)
        .min(MAX_BASH_OUTPUT_LIMIT);

    // Resolve the shell up front. On Windows this can fail when no POSIX
    // shell is installed; surface that as an actionable warning + error rather
    // than a cryptic spawn failure for a missing `bash`.
    let argv = match super::posix_shell::shell_argv(params.command) {
        Ok(argv) => argv,
        Err(message) => {
            context.note_warning();
            return Err(message);
        }
    };

    let output = run_command(SandboxCommand {
        argv,
        cwd,
        timeout_ms,
        max_output_chars: output_limit,
        profile,
    })
    .await
    .inspect_err(|error| {
        // Surface sandbox-availability failures (bwrap missing, kernel
        // refused namespaces, etc.) as a run warning so the run completes
        // with warnings rather than as a generic tool failure. The sandbox runner uses the
        // "Sandboxed shell is unavailable" sentinel prefix for exactly
        // this case; other errors stay as plain tool failures.
        if error.starts_with("Sandboxed shell is unavailable") {
            context.note_warning();
        }
    })
    .map_err(|error| augment_timeout_error(error, timeout_ms, explicit_timeout))?;

    Ok(serde_json::json!({
        "cwd": agent_path_string(&output.cwd),
        "exitCode": output.exit_code,
        "success": output.success,
        "stdout": output.stdout,
        "stderr": output.stderr
    }))
}

/// Turn a bare sandbox timeout into an actionable error the model can recover
/// from. A plain "timed out after N ms" is a dead end — the caller can't tell
/// that a larger `timeoutMs` exists or what its ceiling is. When the run timed
/// out below the cap, point at `timeoutMs` (distinguishing the default from an
/// explicitly requested value); when it was already at the cap, tell the model
/// to narrow the command instead of waiting longer. Non-timeout errors pass
/// through unchanged.
fn augment_timeout_error(error: String, timeout_ms: u64, explicit_timeout: bool) -> String {
    if !error.contains("timed out after") {
        return error;
    }
    if timeout_ms < MAX_BASH_TIMEOUT_MS {
        let which = if explicit_timeout {
            "requested"
        } else {
            "default"
        };
        format!(
            "{error}. That was the {which} timeout; for long-running work \
             (builds, large test suites) re-run with a higher `timeoutMs` \
             (up to {MAX_BASH_TIMEOUT_MS} ms)."
        )
    } else {
        format!(
            "{error}. That is the maximum `timeoutMs` ({MAX_BASH_TIMEOUT_MS} ms); \
             narrow the command (e.g. a single package or a test subset) so it \
             finishes within the limit."
        )
    }
}

fn filesystem_policy(context: &ToolExecutionContext) -> Result<EffectiveFilesystemPolicy, String> {
    let workspace = ensure_workspace_root(context)?;
    let mut grants = context.run_context.execution.filesystem.extra_paths.clone();
    grants.extend(context.session_grants_snapshot());
    EffectiveFilesystemPolicy::from_config(
        &workspace,
        &grants,
        crate::paths::real_home().as_deref(),
    )
}

fn ensure_workspace_root(context: &ToolExecutionContext) -> Result<PathBuf, String> {
    let workspace_root = context.run_context.workspace_root.clone();

    // Create workspace root and pre-seed .clai/memory/ (including journal/)
    // so agents can use it immediately without spending a tool call.
    let memory_dir = workspace_root.join(".clai").join("memory");
    let journal_dir = memory_dir.join("journal");
    fs::create_dir_all(&journal_dir).map_err(|e| {
        format!(
            "Failed to create agent workspace {}: {}",
            workspace_root.display(),
            e
        )
    })?;

    Ok(workspace_root)
}

fn sandbox_profile(
    context: &ToolExecutionContext,
    workspace_root: PathBuf,
    filesystem: EffectiveFilesystemPolicy,
    sandbox: &crate::assistant::sandbox::SandboxStatus,
) -> Result<SandboxProfile, String> {
    let network = sandbox_network(
        context.run_context.execution.sandbox.network_access.mode,
        sandbox,
    )?;

    let session_bus = match context.run_context.execution.sandbox.session_bus {
        SandboxSessionBusConfig::Deny => SandboxSessionBusMode::Deny,
        SandboxSessionBusConfig::Allow => SandboxSessionBusMode::Allow,
    };

    let scratch_tmp = crate::assistant::sandbox::session_scratch(&workspace_root);

    let env_home = filesystem
        .home
        .as_deref()
        .unwrap_or(&filesystem.workspace_root);
    Ok(SandboxProfile {
        env: SandboxEnv::filtered_from_current(env_home, session_bus),
        filesystem,
        network,
        session_bus,
        scratch_tmp,
    })
}

/// The shell's network for a mode. A stored Allowlist where the sandbox
/// cannot enforce it (macOS for now, a hand-edited or copied config) is an
/// error, never the host network.
fn sandbox_network(
    mode: NetworkMode,
    sandbox: &crate::assistant::sandbox::SandboxStatus,
) -> Result<SandboxNetworkMode, String> {
    match mode {
        NetworkMode::Off => Ok(SandboxNetworkMode::Disabled),
        NetworkMode::HostNetwork => Ok(SandboxNetworkMode::Host),
        // Shell commands keep the host network in Allowlist until the sandbox
        // routes them through the network proxy. Without a sandbox nothing is
        // enforced in any mode (D17), so nothing is refused either.
        NetworkMode::Allowlist if sandbox.allowlist_available || !sandbox.available => {
            Ok(SandboxNetworkMode::Host)
        }
        NetworkMode::Allowlist => Err(format!(
            "Allowlist network mode: {}. Choose Off or Host network in the agent's Access \
             settings.",
            sandbox
                .allowlist_unavailable_reason
                .as_deref()
                .unwrap_or("not available with this sandbox")
        )),
    }
}

/// The tool error for a denied ask-first request: what was denied, the rule
/// each part matched, and the user's reason when there is one.
pub(crate) fn ask_first_denied_message(rows: &[AskFirstRow], reason: Option<&str>) -> String {
    let denied = rows
        .iter()
        .map(|row| format!("`{}` (ask-first rule `{}`)", row.text, row.rule))
        .collect::<Vec<_>>()
        .join(", ");
    let mut message = format!("User denied {denied}");
    if let Some(reason) = reason.map(str::trim).filter(|reason| !reason.is_empty()) {
        message.push_str(&format!(". Reason: {reason}"));
    }
    message.push('.');
    message
}

/// What `bash_exec` does with a command.
#[derive(Debug)]
pub(crate) enum PolicyResult {
    /// Run it.
    Allow,
    /// Ask the user first. Holds every row of the command that matched an
    /// ask-first rule.
    AskFirst(Vec<AskFirstRow>),
}

/// Checks `command` against the agent's ask-first list. Only Restricted
/// agents have one in force: Full checks nothing. `run_removed` holds the
/// rules "Always allow" removed earlier in this run, which the run's
/// settings snapshot still lists.
pub(crate) fn evaluate_command_policy(
    execution: &ExecutionCapabilityConfig,
    command: &str,
    run_removed: &[String],
) -> PolicyResult {
    use crate::assistant::tools::command_splitter::{split_command, Segment};

    if !matches!(execution.shell.mode, ShellAccessMode::Restricted) {
        return PolicyResult::Allow;
    }
    let rules: Vec<String> = execution
        .shell
        .ask_first_command_prefixes
        .iter()
        .filter(|rule| {
            !run_removed
                .iter()
                .any(|removed| removed.trim() == rule.trim())
        })
        .cloned()
        .collect();

    // No segments means nothing runs but shell grammar and comments.
    let segments = split_command(command);
    // A row the splitter could not name or parse may hide what runs in any
    // row of the input (`x='a[$(rm -rf y)]'; echo $((x))`), so the rules
    // are also read against every word of each row.
    let any_exact = segments
        .iter()
        .any(|s| matches!(s, Segment::Exact(_) | Segment::Unparsed(_)));
    let rows: Vec<AskFirstRow> = segments
        .iter()
        .flat_map(|segment| {
            let text = segment.text();
            matching_rules(&rules, text, any_exact)
                .into_iter()
                .map(|rule| AskFirstRow {
                    text: text.to_string(),
                    rule,
                })
        })
        .collect();

    if rows.is_empty() {
        PolicyResult::Allow
    } else {
        PolicyResult::AskFirst(rows)
    }
}

/// Every rule of `rules` that names a command the row `text` runs, trimmed,
/// once each, in list order. "Always allow" removes all of them, so the row
/// does not ask again through another rule.
///
/// A rule matches the row as written, and also when the command is run by
/// path or after assignments (`FOO=1 /usr/bin/rm`). The row is also read
/// as the head it runs a command with and the command that head runs:
/// `timeout 60 rm -rf x` matches `rm` and `timeout`, `xargs rm` matches
/// `rm` and `xargs`. Every command a misread option could hide is read
/// too: `xargs -0I rm echo` matches `rm`, and so are the commands `find`
/// runs for its matches and the method `gh api` requests. When `any_exact`
/// is set, some row of the input could not be named, so every run of words
/// is read.
fn matching_rules(rules: &[String], text: &str, any_exact: bool) -> Vec<String> {
    use crate::assistant::tools::prefix_detector::{
        by_any_reading, by_argument_command, by_find_action, by_head_name, by_run_command,
        canonicalize_for_deny, gh_api_method_forms,
    };

    let mut readings: Vec<String> = std::iter::once(text.to_string())
        .chain(by_head_name(text))
        .chain(by_run_command(text))
        .chain(by_argument_command(text))
        .chain(by_any_reading(text))
        .collect();
    // `find` first, so the method of a `gh api` that `find` runs is read too.
    let find_actions: Vec<String> = readings
        .iter()
        .map(String::as_str)
        .flat_map(by_find_action)
        .collect();
    readings.extend(find_actions);
    let gh_api_methods: Vec<String> = readings
        .iter()
        .map(String::as_str)
        .flat_map(gh_api_method_forms)
        .collect();
    readings.extend(gh_api_methods);
    // A rule cannot hide behind leading assignments or global options either:
    // `FOO=bar rm` reads as `rm`, `git -C repo push` as `git push`.
    let canonical: Vec<String> = readings
        .iter()
        .filter_map(|reading| canonicalize_for_deny(reading))
        .collect();
    readings.extend(canonical);
    if any_exact {
        readings.extend(word_runs(text));
    }

    let mut matched: Vec<String> = Vec::new();
    for rule in rules.iter().map(|rule| rule.trim()) {
        if !rule.is_empty()
            && !matched.iter().any(|seen| seen == rule)
            && readings
                .iter()
                .any(|reading| matches_prefix_literal(rule, reading))
        {
            matched.push(rule.to_string());
        }
    }
    matched
}

/// Words a rule may span in [`word_runs`].
const WORD_RUN: usize = 16;

/// Each word of `text` with up to [`WORD_RUN`] words after it, joined by single spaces,
/// with quotes and backslashes removed and the directory dropped from the
/// word: `x='a[$(/bin/rm -rf y)]'` gives `x`, `a`, `rm -rf y`, `-rf y`
/// and `y`.
fn word_runs(text: &str) -> impl Iterator<Item = String> {
    let unquoted: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let words: Vec<String> = unquoted
        .split(|c: char| !(c.is_alphanumeric() || "_-./+@%,:~".contains(c)))
        .filter(|word| !word.is_empty())
        .map(|word| word.rsplit('/').next().unwrap_or(word).to_string())
        .collect();
    (0..words.len()).map(move |at| words[at..words.len().min(at + WORD_RUN)].join(" "))
}

/// Cleans up an abandoned permission request when the approval-wait future
/// is dropped without a user decision — the run being cancelled, or the
/// run ending while this wait was orphaned by a CLI transport drop (the
/// rmcp session worker keeps the future alive past the dropped
/// connection; `BindingGuard` reaps it at run end, which drops this
/// future). The guard then removes the still-pending registry entry,
/// tells the frontend to drop the now-useless approval card, and cancels
/// the run so the model cannot continue without the missing decision
/// (a no-op when the run already ended). Disarmed on a normal decision or
/// explicit supersede, where the caller already removed/replaced the entry.
///
/// Cleanup is async (registry lock) so it's spawned onto the app runtime;
/// `Drop` can't await. `take` is a no-op if the entry was already removed
/// (e.g. a decision raced in), so the guard is safe even if it fires late.
struct AbandonedApprovalGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedApprovalGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_approvals.take(&self.request_id).await {
            crate::commands::permissions::emit_permission_resolved(&self.app, &self.request_id);
            crate::commands::permissions::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedApprovalGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_approvals.take(&request_id).await {
                crate::commands::permissions::emit_permission_resolved(&app, &request_id);
                crate::commands::permissions::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

/// Asks the user about a command that matched the ask-first list. Registers
/// the request in app state, emits the request and attention events, waits
/// for the decision, and resolves to `Ok(())` when the command may run or
/// `Err(_)` carrying the user's reason on a denial. If the wait is abandoned
/// without a decision, `AbandonedApprovalGuard` cancels the run so the model
/// does not continue around a missing human decision. "Always allow" is saved
/// on the agent by [`crate::commands::permissions::submit_permission_decision`]
/// before the decision is delivered.
async fn await_ask_first(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    command: &str,
    rows: Vec<AskFirstRow>,
) -> Result<(), String> {
    use crate::commands::permissions::{
        emit_attention, PendingApprovalOutcome, PermissionRequest, PERMISSION_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = Some(deps.workspace_id.clone());
    let agent_id = context.run_context.automation_id.clone();

    // The constructor resolves where "Always allow" would be saved — in this
    // workspace for its Main, or on a shared definition in force wherever
    // that teammate works.
    let request = PermissionRequest::for_agent(
        &app_state,
        workspace_id.clone(),
        agent_id,
        command.to_string(),
        rows.clone(),
    );
    let request_id = request.request_id.clone();
    let rules = request.rules();

    // Supersede: if a previous request for this exact run + command is
    // still pending, it is an orphan — its CLI transport dropped mid-call
    // and the model is now re-asking. Replace it (and its UI card) with
    // the fresh request instead of stacking a duplicate the user can no
    // longer meaningfully answer. The stale waiter receives an explicit
    // supersede outcome (no run cancellation — this run is alive and
    // waiting on the NEW request).
    for stale in app_state
        .pending_approvals
        .take_superseded(&context.run_id, command)
        .await
    {
        crate::commands::permissions::emit_permission_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_approvals
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PERMISSION_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit permission request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // Cleans up if this future is abandoned before a decision (CLI transport
    // drop mid-call, or run cancellation): clears the pending entry and tells
    // the frontend to drop the now-useless approval card. Disarmed below on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedApprovalGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    let decision = match rx.await {
        Ok(PendingApprovalOutcome::Decision(decision)) => {
            // The submit command already removed the registry entry and
            // emitted `permissions://resolved`.
            abandon_guard.disarm();
            decision
        }
        Ok(PendingApprovalOutcome::Superseded) => {
            // A fresh registration for the same run + command replaced
            // this orphaned wait after a transport drop. This stale future
            // is intentionally ignored: no notice, no warning, no cancel.
            abandon_guard.disarm();
            let msg = "Permission request was superseded by a newer request \
                       for the same command before a decision was made";
            return Err(msg.to_string());
        }
        Err(_) if context.cancel_token.is_cancelled() => {
            return super::cancel_run_and_park(&context.cancel_token).await;
        }
        Err(_) => {
            return abandon_guard.expire_and_stop().await;
        }
    };

    apply_ask_first_decision(context, &rows, &rules, decision)
}

/// Applies the user's answer to the command waiting on it: `Ok(())` runs
/// it, and a denial is the tool error. `rules` are the rules the command
/// matched, which "Always allow" removed from the agent's list.
fn apply_ask_first_decision(
    context: &ToolExecutionContext,
    rows: &[AskFirstRow],
    rules: &[String],
    decision: AskFirstDecision,
) -> Result<(), String> {
    match decision {
        AskFirstDecision::AllowOnce => Ok(()),
        AskFirstDecision::AlwaysAllow => {
            // This run reads its settings from a snapshot taken at start, so
            // the removal saved on the agent would not reach it.
            context.add_run_removed_ask_first(rules);
            Ok(())
        }
        AskFirstDecision::Deny { reason } => {
            context.note_warning();
            Err(ask_first_denied_message(rows, reason.as_deref()))
        }
    }
}

async fn execute_fs_request_grant(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    params: FsRequestGrantParams,
) -> Result<serde_json::Value, String> {
    let canonical =
        canonicalize_requested_path(&params.path, crate::paths::real_home().as_deref())?;
    let canonical_str = agent_path_string(&canonical);

    // If the path is already covered (by extra_paths, the preset, or an
    // earlier session grant), short-circuit — no user prompt, just say yes.
    // This keeps repeated requests cheap and avoids modal spam when the LLM
    // forgets it already has the grant.
    let filesystem = filesystem_policy(context)?;
    if filesystem.covers(&canonical, params.access) {
        return Ok(serde_json::json!({
            "granted": true,
            "path": canonical_str,
            "access": access_to_str(params.access),
            "scope": "already-granted",
        }));
    }

    let request = crate::commands::path_grants::PathGrantRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: Some(deps.workspace_id.clone()),
        agent_id: context.run_context.automation_id.clone(),
        agent_name: None,
        requested_path: canonical_str.clone(),
        requested_access: params.access,
        reason: params.reason.clone(),
    };

    let decision = await_path_grant_decision(deps, context, request).await?;

    match decision {
        crate::commands::path_grants::PathGrantDecision::Deny => {
            let msg = format!("User denied path grant for `{}`", canonical_str);
            context.note_warning();
            Err(msg)
        }
        crate::commands::path_grants::PathGrantDecision::AllowOnce { path, access } => {
            apply_session_grant(context, &path, access);
            context.note_warning();
            Ok(serde_json::json!({
                "granted": true,
                "path": agent_path_string(std::path::Path::new(&path)),
                "access": access_to_str(access),
                "scope": "once",
                "note": "Valid for the current turn only — NOT retained for later turns. If you need this path again in a future turn, request it again, or ask the user to grant it always.",
            }))
        }
        crate::commands::path_grants::PathGrantDecision::AllowAlways {
            path,
            access,
            scope: _,
        } => {
            // The submit handler already persisted to the agent's DB row;
            // we only need to make the grant visible in the *current* run.
            apply_session_grant(context, &path, access);
            context.note_warning();
            Ok(serde_json::json!({
                "granted": true,
                "path": agent_path_string(std::path::Path::new(&path)),
                "access": access_to_str(access),
                "scope": "always",
            }))
        }
    }
}

/// Turn the agent's requested path into the identity the compiled policy
/// reasons about. `~` expands to the host home (not the process `HOME`, which
/// differs under Flatpak) and resolution goes through
/// [`resolve_existing`], the same resolver `EffectiveFilesystemPolicy::compile`
/// uses, so coverage checks and the persisted grant agree with enforcement.
fn canonicalize_requested_path(input: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("fs_request_grant requires a non-empty path".to_string());
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        home.ok_or_else(|| "Cannot expand ~ in path: home directory is unknown".to_string())?
            .join(rest)
    } else if trimmed == "~" {
        home.ok_or_else(|| "Cannot expand ~ in path: home directory is unknown".to_string())?
            .to_path_buf()
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "fs_request_grant requires an absolute or ~-prefixed path; got `{}`",
            input
        ));
    }
    resolve_existing(&expanded).map_err(|error| {
        format!(
            "fs_request_grant requires an existing path because the shell sandbox cannot bind a nonexistent target: {}. Request an existing parent directory instead ({})",
            expanded.display(),
            error
        )
    })
}

fn apply_session_grant(context: &ToolExecutionContext, path: &str, access: FilesystemPathAccess) {
    context.add_session_grant(FilesystemPathGrant {
        path: path.to_string(),
        access,
        origin: Some(GrantOrigin::Approval {
            reason: String::new(),
            granted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        }),
    });
}

fn access_to_str(access: FilesystemPathAccess) -> &'static str {
    match access {
        FilesystemPathAccess::ReadOnly => "read_only",
        FilesystemPathAccess::ReadWrite => "read_write",
    }
}

/// Path-grant analogue of [`AbandonedApprovalGuard`]: clears a pending
/// filesystem path-grant request, drops its card, and cancels the run when
/// the approval-wait future is abandoned (run cancellation, or run-end
/// reaping of a wait orphaned by a CLI transport drop).
struct AbandonedPathGrantGuard {
    app: tauri::AppHandle,
    cancel_token: tokio_util::sync::CancellationToken,
    request_id: String,
    workspace_id: Option<String>,
    armed: bool,
}

impl AbandonedPathGrantGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn expire_and_stop<T>(&mut self) -> T {
        use tauri::Manager;

        let state = self.app.state::<crate::AppState>();
        if let Some((_, remaining)) = state.pending_path_grants.take(&self.request_id).await {
            crate::commands::path_grants::emit_path_grant_resolved(&self.app, &self.request_id);
            crate::commands::path_grants::emit_attention(
                &self.app,
                self.workspace_id.clone(),
                remaining,
            );
        }
        self.armed = false;
        super::cancel_run_and_park(&self.cancel_token).await
    }
}

impl Drop for AbandonedPathGrantGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancel_token.cancel();
        use tauri::Manager;
        let app = self.app.clone();
        let request_id = std::mem::take(&mut self.request_id);
        let workspace_id = self.workspace_id.take();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<crate::AppState>();
            if let Some((_, remaining)) = state.pending_path_grants.take(&request_id).await {
                crate::commands::path_grants::emit_path_grant_resolved(&app, &request_id);
                crate::commands::path_grants::emit_attention(&app, workspace_id, remaining);
            }
        });
    }
}

/// Awaits the user's decision on a path-grant request. Mirrors
/// [`await_ask_first`] for shape; the only differences are the
/// request type and the registry it talks to.
async fn await_path_grant_decision(
    deps: &crate::assistant::engine::AssistantDeps,
    context: &ToolExecutionContext,
    request: crate::commands::path_grants::PathGrantRequest,
) -> Result<crate::commands::path_grants::PathGrantDecision, String> {
    use crate::commands::path_grants::{
        emit_attention, PendingPathGrantOutcome, PATH_GRANT_REQUEST_EVENT,
    };
    use tauri::{Emitter, Manager};

    let app_state = deps.app.state::<crate::AppState>();
    let workspace_id = Some(deps.workspace_id.clone());
    let request_id = request.request_id.clone();

    // Supersede a stale orphaned request for the same run + path + access
    // (the model re-asked after a CLI transport drop). See the analogous
    // block in `await_ask_first` for the full rationale.
    for stale in app_state
        .pending_path_grants
        .take_superseded(
            &context.run_id,
            &request.requested_path,
            request.requested_access,
        )
        .await
    {
        crate::commands::path_grants::emit_path_grant_resolved(&deps.app, &stale.request_id);
        emit_attention(&deps.app, stale.workspace_id.clone(), stale.remaining);
    }

    let (rx, count) = app_state
        .pending_path_grants
        .register(request.clone(), context.run_id.clone())
        .await;

    if let Err(e) = deps.app.emit(PATH_GRANT_REQUEST_EVENT, &request) {
        tracing::warn!("Failed to emit path-grant request event: {}", e);
    }
    emit_attention(&deps.app, workspace_id.clone(), count);

    // See `AbandonedApprovalGuard`: clears the pending entry and drops the
    // card if this future is abandoned before a decision. Disarmed on a
    // normal decision and explicit supersede.
    let mut abandon_guard = AbandonedPathGrantGuard {
        app: deps.app.clone(),
        cancel_token: context.cancel_token.clone(),
        request_id: request_id.clone(),
        workspace_id: workspace_id.clone(),
        armed: true,
    };

    match rx.await {
        Ok(PendingPathGrantOutcome::Decision(decision)) => {
            abandon_guard.disarm();
            Ok(decision)
        }
        Ok(PendingPathGrantOutcome::Superseded) => {
            // A fresh registration for the same run + path + access
            // replaced this orphaned wait after a transport drop. This
            // stale future is intentionally ignored.
            abandon_guard.disarm();
            let msg = "Path-grant request was superseded by a newer request \
                       for the same path before a decision was made"
                .to_string();
            Err(msg)
        }
        Err(_) if context.cancel_token.is_cancelled() => {
            super::cancel_run_and_park(&context.cancel_token).await
        }
        Err(_) => abandon_guard.expire_and_stop().await,
    }
}

/// Literal word-boundary prefix test: `command` equals `p`, or starts with
/// `p` followed by whitespace (so `git log` matches `git log --oneline` but
/// not `git logs`).
fn matches_prefix_literal(p: &str, command: &str) -> bool {
    command
        .strip_prefix(p)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_whitespace()))
}

// =============================================================================
// Web tools
// =============================================================================

/// Which hosts the web tools may reach for one agent.
#[derive(Debug)]
enum WebGate {
    /// No sandbox on this PC, so no network control at all.
    Unchecked,
    /// Host network: any public host.
    AnyHost,
    /// Allowlist: only the hosts this policy allows, on every redirect hop.
    Allowlist(HostLists),
}

fn web_gate(context: &ToolExecutionContext) -> Result<WebGate, String> {
    web_access(
        &context.run_context.execution.sandbox.network_access,
        &crate::assistant::sandbox::status(),
    )
}

fn web_access(
    network: &NetworkPolicy,
    sandbox: &crate::assistant::sandbox::SandboxStatus,
) -> Result<WebGate, String> {
    if !sandbox.available {
        return Ok(WebGate::Unchecked);
    }
    match network.mode {
        NetworkMode::Off => Err("Network access is off for this agent".to_string()),
        NetworkMode::Allowlist => Ok(WebGate::Allowlist(HostLists::new(
            &network.allow_domains,
            &network.deny_domains,
        ))),
        NetworkMode::HostNetwork => Ok(WebGate::AnyHost),
    }
}

/// `host` as the URL carries it: lower case, punycode, IPv6 in brackets.
fn check_web_host(gate: &WebGate, host: &str) -> Result<(), String> {
    let WebGate::Allowlist(lists) = gate else {
        return Ok(());
    };
    match lists.decide(host) {
        HostDecision::Allowed => Ok(()),
        HostDecision::Denied { entry } => Err(format!(
            "`{host}` is on this agent's network deny list (entry `{entry}`). The user can \
             change the list in the agent's Access settings."
        )),
        HostDecision::Unlisted => Err(format!(
            "`{host}` is not on this agent's network allowlist. Call `net_request_access` to ask \
             the user, or they can add it in the agent's Access settings."
        )),
    }
}

const MAX_WEB_SEARCH_REDIRECTS: usize = 3;

/// web_search calls only its provider's fixed endpoint, which is allowed
/// implicitly; a redirect may not leave the provider or drop https.
fn provider_redirect_allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
}

fn web_search_redirect_policy(gate: &WebGate) -> reqwest::redirect::Policy {
    if matches!(gate, WebGate::Unchecked) {
        return reqwest::redirect::Policy::limited(MAX_WEB_SEARCH_REDIRECTS);
    }
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_WEB_SEARCH_REDIRECTS {
            attempt.error("too many redirects")
        } else if provider_redirect_allowed(attempt.url()) {
            attempt.follow()
        } else {
            let message = format!("redirect to `{}` leaves the search provider", attempt.url());
            attempt.error(message)
        }
    })
}

async fn execute_web_search(
    gate: WebGate,
    params: WebSearchParams,
) -> Result<serde_json::Value, String> {
    let max_results = params
        .max_results
        .unwrap_or(DEFAULT_WEB_SEARCH_MAX_RESULTS)
        .min(MAX_WEB_SEARCH_MAX_RESULTS);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(WEB_SEARCH_TIMEOUT_MS))
        .redirect(web_search_redirect_policy(&gate))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", &params.query)])
        .header("User-Agent", "CLAI/1.0")
        .send()
        .await
        .map_err(|e| format!("Web search request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Web search returned HTTP {}",
            response.status().as_u16()
        ));
    }

    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read search response: {}", e))?;

    let results = parse_duckduckgo_results(&html, max_results);

    Ok(serde_json::json!({
        "query": params.query,
        "results": results,
        "resultCount": results.len()
    }))
}

fn parse_duckduckgo_results(html: &str, max_results: usize) -> Vec<serde_json::Value> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let result_selector = Selector::parse(".result").unwrap();
    let link_selector = Selector::parse(".result__a").unwrap();
    let snippet_selector = Selector::parse(".result__snippet").unwrap();

    let mut results = Vec::new();

    for element in document.select(&result_selector) {
        if results.len() >= max_results {
            break;
        }

        let Some(link) = element.select(&link_selector).next() else {
            continue;
        };

        let title = link.text().collect::<String>().trim().to_string();
        if title.is_empty() {
            continue;
        }

        // DuckDuckGo HTML wraps URLs through a redirect; extract the actual URL
        let href = link.value().attr("href").unwrap_or_default();
        let url = extract_ddg_url(href);
        if url.is_empty() {
            continue;
        }

        let snippet = element
            .select(&snippet_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(serde_json::json!({
            "title": title,
            "url": url,
            "snippet": snippet
        }));
    }

    results
}

/// Extract the actual URL from DuckDuckGo's redirect wrapper.
/// DDG HTML results use hrefs like `//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com&rut=...`
fn extract_ddg_url(href: &str) -> String {
    if let Some(pos) = href.find("uddg=") {
        let encoded = &href[pos + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        urldecode(encoded)
    } else if href.starts_with("http") {
        href.to_string()
    } else {
        String::new()
    }
}

/// Minimal percent-decoding for URL extraction.
fn urldecode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().unwrap_or(b'0');
            let lo = chars.next().unwrap_or(b'0');
            if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                result.push((h << 4 | l) as char);
            } else {
                result.push('%');
                result.push(hi as char);
                result.push(lo as char);
            }
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

async fn execute_web_fetch(
    gate: WebGate,
    params: WebFetchParams,
) -> Result<serde_json::Value, String> {
    let content_limit = params
        .max_content_chars
        .unwrap_or(DEFAULT_WEB_FETCH_CONTENT_LIMIT)
        .min(MAX_WEB_FETCH_CONTENT_LIMIT);
    let timeout_ms = params
        .timeout_ms
        .unwrap_or(DEFAULT_WEB_FETCH_TIMEOUT_MS)
        .min(MAX_WEB_FETCH_TIMEOUT_MS);

    let url = parse_web_fetch_url(params.url.trim())?;
    let response = fetch_public_url(url, timeout_ms, &gate).await?;
    let final_url = response.url().clone();

    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(format!("Fetch returned HTTP {}", status));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body_limit = web_fetch_body_limit(content_limit);
    let (body, body_truncated) = read_limited_web_fetch_body(response, body_limit).await?;

    // Convert HTML to markdown, or return plain text as-is
    let markdown = if content_type.contains("text/html") || content_type.contains("xhtml") {
        html_to_markdown(&body)
    } else {
        body
    };

    let markdown_truncated = markdown.len() > content_limit;
    let content = if markdown_truncated {
        let chars: Vec<char> = markdown.chars().collect();
        let end = content_limit.min(chars.len());
        chars[..end].iter().collect::<String>()
    } else {
        markdown
    };
    let truncated = body_truncated || markdown_truncated;

    Ok(serde_json::json!({
        "url": final_url.as_str(),
        "contentType": content_type,
        "content": content,
        "truncated": truncated,
        "contentLength": content.len()
    }))
}

fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| {
        // Fallback: strip tags with scraper and return plain text
        let document = scraper::Html::parse_document(html);
        document.root_element().text().collect::<String>()
    })
}

fn parse_web_fetch_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "URL must be an absolute http:// or https:// URL".to_string())?;
    validate_web_fetch_scheme(&url)?;
    Ok(url)
}

fn validate_web_fetch_scheme(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        _ => Err("URL must start with http:// or https://".to_string()),
    }
}

async fn fetch_public_url(
    mut url: reqwest::Url,
    timeout_ms: u64,
    gate: &WebGate,
) -> Result<reqwest::Response, String> {
    // A redirect must never downgrade the transport: if the original request
    // was https, refuse to follow a redirect to plain http (a MITM/attacker
    // could otherwise strip TLS by 30x-ing to http).
    let started_https = url.scheme() == "https";
    let mut redirects_followed = 0;
    loop {
        let dns_override = resolve_public_web_fetch_target(&url, gate).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none());

        if let Some((host, addrs)) = &dns_override {
            builder = builder.resolve_to_addrs(host, addrs);
        }

        let client = builder
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let response = client
            .get(url.clone())
            .header("User-Agent", "CLAI/1.0")
            .header("Accept", "text/html, text/plain, application/xhtml+xml")
            .send()
            .await
            .map_err(|e| format!("Fetch failed: {}", e))?;

        if is_followable_redirect(response.status()) {
            if redirects_followed >= MAX_WEB_FETCH_REDIRECTS {
                return Err("Fetch exceeded redirect limit".to_string());
            }
            redirects_followed += 1;

            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| {
                    format!(
                        "Fetch returned HTTP {} without a Location header",
                        response.status().as_u16()
                    )
                })?
                .to_str()
                .map_err(|_| "Redirect location is not valid UTF-8".to_string())?;
            url = url
                .join(location)
                .map_err(|e| format!("Invalid redirect location: {}", e))?;
            validate_web_fetch_scheme(&url)?;
            if started_https && url.scheme() != "https" {
                return Err(
                    "Refusing to follow an https->http redirect (transport downgrade)".to_string(),
                );
            }
            continue;
        }

        return Ok(response);
    }
}

fn is_followable_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Runs on every redirect hop.
async fn resolve_public_web_fetch_target(
    url: &reqwest::Url,
    gate: &WebGate,
) -> Result<Option<(String, Vec<SocketAddr>)>, String> {
    validate_web_fetch_scheme(url)?;
    let host = url
        .host_str()
        .ok_or_else(|| "URL must include a host".to_string())?;
    let host_literal = strip_ipv6_url_brackets(host);
    let normalized_host = host_literal.trim_end_matches('.').to_ascii_lowercase();
    if is_blocked_web_fetch_hostname(&normalized_host) {
        return Err("Fetching private/local URLs is not allowed".to_string());
    }
    check_web_host(gate, host)?;

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL must include a valid port".to_string())?;

    if let Ok(ip) = host_literal.parse::<IpAddr>() {
        reject_private_web_fetch_ip(ip)?;
        return Ok(None);
    }

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("Failed to resolve URL host: {}", e))?
        .collect();
    if addrs.is_empty() {
        return Err("URL host did not resolve to any address".to_string());
    }

    reject_private_web_fetch_addrs(&addrs)?;

    // Pin the public DNS result into reqwest for this request so a hostname
    // cannot validate as public and then resolve privately during connect.
    Ok(Some((host.to_string(), addrs)))
}

fn web_fetch_body_limit(content_limit: usize) -> usize {
    content_limit
        .saturating_mul(4)
        .min(MAX_WEB_FETCH_BODY_BYTES)
}

async fn read_limited_web_fetch_body(
    response: reqwest::Response,
    body_limit: usize,
) -> Result<(String, bool), String> {
    let mut body = Vec::with_capacity(body_limit.min(16 * 1024));
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Failed to read response body: {}", e))?;
        if append_limited_web_fetch_body_chunk(&mut body, &chunk, body_limit) {
            return Ok((String::from_utf8_lossy(&body).into_owned(), true));
        }
    }

    Ok((String::from_utf8_lossy(&body).into_owned(), false))
}

fn append_limited_web_fetch_body_chunk(body: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    let remaining = limit.saturating_sub(body.len());
    if remaining == 0 {
        return true;
    }

    if chunk.len() > remaining {
        body.extend_from_slice(&chunk[..remaining]);
        return true;
    }

    body.extend_from_slice(chunk);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::run_context::RunContext;
    use tempfile::tempdir;

    fn context_with_grants(
        workspace_root: &std::path::Path,
        extra_paths: Vec<FilesystemPathGrant>,
    ) -> ToolExecutionContext {
        let mut execution = ExecutionCapabilityConfig::default();
        execution.filesystem.extra_paths = extra_paths;
        ToolExecutionContext {
            session_id: "session".to_string(),
            run_id: "run".to_string(),
            tool_call_id: None,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            run_context: std::sync::Arc::new(RunContext {
                automation_id: Some("agent".to_string()),
                execution,
                workspace_root: workspace_root.to_path_buf(),
                ..RunContext::default()
            }),
            had_warnings: Default::default(),
            session_grants: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            run_removed_ask_first: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    #[test]
    fn execution_context_compiles_configured_and_session_grants() {
        let workspace = tempdir().unwrap();
        let external = tempdir().unwrap();
        let context = context_with_grants(
            workspace.path(),
            vec![FilesystemPathGrant {
                path: workspace.path().join("docs").display().to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            }],
        );
        context
            .session_grants
            .lock()
            .unwrap()
            .push(FilesystemPathGrant {
                path: external.path().display().to_string(),
                access: FilesystemPathAccess::ReadOnly,
                origin: None,
            });
        let policy = filesystem_policy(&context).unwrap();
        assert_eq!(
            policy.workspace_root,
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(policy.path_grants.len(), 1);
        assert!(policy.covers(
            &external.path().canonicalize().unwrap(),
            FilesystemPathAccess::ReadOnly
        ));
        assert!(!policy.covers(
            &external.path().canonicalize().unwrap(),
            FilesystemPathAccess::ReadWrite
        ));
        assert!(policy.covers(
            &policy.workspace_root.join("docs"),
            FilesystemPathAccess::ReadWrite
        ));
    }

    // ------------------------------------------------------------------
    // canonicalize_requested_path
    // ------------------------------------------------------------------

    #[test]
    fn canonicalize_rejects_empty_path() {
        let err = canonicalize_requested_path("   ", None).unwrap_err();
        assert!(err.contains("non-empty"));
    }

    #[test]
    fn canonicalize_rejects_relative_paths() {
        let err = canonicalize_requested_path("relative/path", None).unwrap_err();
        assert!(err.contains("absolute"));
    }

    #[test]
    fn canonicalize_expands_tilde_with_supplied_home() {
        let temp = tempdir().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();

        let resolved = canonicalize_requested_path("~", Some(temp.path())).unwrap();
        assert_eq!(resolved, temp.path().canonicalize().unwrap());

        let resolved = canonicalize_requested_path("~/nested", Some(temp.path())).unwrap();
        assert_eq!(resolved, nested.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_rejects_tilde_without_known_home() {
        let err = canonicalize_requested_path("~/anything", None).unwrap_err();
        assert!(err.contains("home directory is unknown"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_resolves_symlinked_request_to_its_target() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let resolved = canonicalize_requested_path(&link.display().to_string(), None).unwrap();
        assert_eq!(resolved, target.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_rejects_nonexistent_grant_target() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("future-cache");
        let error = canonicalize_requested_path(&missing.display().to_string(), None).unwrap_err();
        assert!(error.contains("requires an existing path"), "{error}");
        assert!(error.contains("existing parent directory"), "{error}");
    }

    #[test]
    fn agent_path_string_normalizes_paths_for_the_agent() {
        // Windows branch is exercised by the windows-latest CI job.
        #[cfg(windows)]
        {
            assert_eq!(
                agent_path_string(std::path::Path::new(r"C:\Users\dev\proj\file.rs")),
                "C:/Users/dev/proj/file.rs"
            );
            // `std::fs::canonicalize` emits a `\\?\` verbatim prefix; strip it.
            assert_eq!(
                agent_path_string(std::path::Path::new(r"\\?\C:\Users\dev\file.rs")),
                "C:/Users/dev/file.rs"
            );
        }
        #[cfg(not(windows))]
        {
            // Unix: returned verbatim (backslash is a legal filename byte).
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/proj/file.rs")),
                "/home/dev/proj/file.rs"
            );
            assert_eq!(
                agent_path_string(std::path::Path::new("/home/dev/we\\ird")),
                "/home/dev/we\\ird"
            );
        }
    }

    #[test]
    fn extract_ddg_url_decodes_redirect_wrapper() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc123";
        assert_eq!(extract_ddg_url(href), "https://example.com/page");
    }

    #[test]
    fn extract_ddg_url_passes_through_direct_urls() {
        assert_eq!(
            extract_ddg_url("https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn extract_ddg_url_returns_empty_for_unknown_format() {
        assert_eq!(extract_ddg_url("/some/relative/path"), "");
    }

    #[test]
    fn urldecode_handles_percent_encoding() {
        assert_eq!(urldecode("hello%20world"), "hello world");
        assert_eq!(
            urldecode("https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("plain"), "plain");
    }

    #[test]
    fn parse_web_fetch_url_rejects_non_absolute_http_urls() {
        assert!(parse_web_fetch_url("/relative/path").is_err());
        assert!(parse_web_fetch_url("ftp://example.com/file").is_err());
    }

    #[test]
    fn web_fetch_body_reader_truncates_chunks_at_limit() {
        let mut body = Vec::new();

        assert!(!append_limited_web_fetch_body_chunk(&mut body, b"hello", 8));
        assert!(append_limited_web_fetch_body_chunk(&mut body, b" world", 8));
        assert_eq!(body, b"hello wo");
    }

    #[test]
    fn web_fetch_body_limit_scales_but_stays_bounded() {
        assert_eq!(web_fetch_body_limit(0), 0);
        assert_eq!(web_fetch_body_limit(1_000), 4_000);
        assert_eq!(web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT), 400_000);
        assert_eq!(
            web_fetch_body_limit(MAX_WEB_FETCH_CONTENT_LIMIT * 10),
            MAX_WEB_FETCH_BODY_BYTES
        );
    }

    #[test]
    fn parse_duckduckgo_results_extracts_from_html() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org&rut=x">Rust Language</a>
            <a class="result__snippet">A systems programming language</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs&rut=y">Docs.rs</a>
            <a class="result__snippet">Rust package documentation</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Rust Language");
        assert_eq!(results[0]["url"], "https://rust-lang.org");
        assert_eq!(results[1]["title"], "Docs.rs");
    }

    #[test]
    fn parse_duckduckgo_results_respects_max_results() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com&rut=x">A</a>
        </div>
        <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fb.com&rut=y">B</a>
        </div>
        "#;

        let results = parse_duckduckgo_results(html, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn html_to_markdown_converts_basic_html() {
        let html = "<html><body><h1>Title</h1><p>Hello <strong>world</strong></p></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("**world**"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http_urls() {
        let params = WebFetchParams {
            url: "ftp://example.com/file".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(WebGate::Unchecked, params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http:// or https://"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_urls() {
        let params = WebFetchParams {
            url: "http://192.168.1.1/admin".to_string(),
            max_content_chars: None,
            timeout_ms: None,
        };
        let result = execute_web_fetch(WebGate::Unchecked, params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("private/local"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_private_ipv6_literals() {
        let url = parse_web_fetch_url("http://[::1]/admin").unwrap();

        let err = resolve_public_web_fetch_target(&url, &WebGate::Unchecked)
            .await
            .unwrap_err();
        assert!(err.contains("private/local"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_redirect_targets_to_private_urls() {
        let start = parse_web_fetch_url("https://example.com/start").unwrap();
        let redirected = start.join("http://127.0.0.1/admin").unwrap();

        let err = resolve_public_web_fetch_target(&redirected, &WebGate::Unchecked)
            .await
            .unwrap_err();
        assert!(err.contains("private/local"));
    }

    // ------------------------------------------------------------------
    // Web tools under the agent's network mode
    // ------------------------------------------------------------------

    fn allowlist(allow: &[&str], deny: &[&str]) -> NetworkPolicy {
        NetworkPolicy {
            mode: NetworkMode::Allowlist,
            allow_domains: allow.iter().map(|entry| entry.to_string()).collect(),
            deny_domains: deny.iter().map(|entry| entry.to_string()).collect(),
        }
    }

    fn allowlist_gate(policy: &NetworkPolicy) -> WebGate {
        WebGate::Allowlist(HostLists::new(&policy.allow_domains, &policy.deny_domains))
    }

    #[test]
    fn web_access_follows_the_mode_where_a_sandbox_exists() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        let mut policy = allowlist(&["github.com"], &[]);
        assert!(matches!(
            web_access(&policy, &linux),
            Ok(WebGate::Allowlist(_))
        ));
        policy.mode = NetworkMode::HostNetwork;
        assert!(matches!(web_access(&policy, &linux), Ok(WebGate::AnyHost)));
        policy.mode = NetworkMode::Off;
        assert_eq!(
            web_access(&policy, &linux).unwrap_err(),
            "Network access is off for this agent"
        );
        // macOS enforces the list in the web tools although its shell cannot.
        policy.mode = NetworkMode::Allowlist;
        let macos = status_for(Some(SandboxBackend::Seatbelt));
        assert!(matches!(
            web_access(&policy, &macos),
            Ok(WebGate::Allowlist(_))
        ));
        // Without a sandbox nothing is checked, a stored Off included.
        policy.mode = NetworkMode::Off;
        assert!(matches!(
            web_access(&policy, &status_for(None)),
            Ok(WebGate::Unchecked)
        ));
    }

    #[tokio::test]
    async fn web_fetch_refuses_an_unlisted_host_naming_it_and_the_way_out() {
        let policy = allowlist(&["github.com"], &[]);
        let url = parse_web_fetch_url("https://example.org/page").unwrap();
        let err = resolve_public_web_fetch_target(&url, &allowlist_gate(&policy))
            .await
            .unwrap_err();
        assert!(
            err.contains("`example.org` is not on this agent's network allowlist"),
            "{err}"
        );
        assert!(err.contains("net_request_access"), "{err}");
    }

    #[tokio::test]
    async fn web_fetch_refuses_a_denied_host_naming_the_entry() {
        let policy = allowlist(&["*.amazonaws.com"], &["*.s3.amazonaws.com"]);
        let url = parse_web_fetch_url("https://bucket.s3.amazonaws.com/x").unwrap();
        let err = resolve_public_web_fetch_target(&url, &allowlist_gate(&policy))
            .await
            .unwrap_err();
        assert!(err.contains("`*.s3.amazonaws.com`"), "{err}");
    }

    #[tokio::test]
    async fn web_fetch_checks_the_allowlist_on_every_redirect_hop() {
        let policy = allowlist(&["github.com"], &[]);
        let start = parse_web_fetch_url("https://github.com/start").unwrap();
        let redirected = start.join("https://example.org/landing").unwrap();
        let err = resolve_public_web_fetch_target(&redirected, &allowlist_gate(&policy))
            .await
            .unwrap_err();
        assert!(err.contains("`example.org`"), "{err}");
    }

    #[tokio::test]
    async fn web_fetch_keeps_the_private_address_filter_in_every_mode() {
        let listed = allowlist(&["10.0.0.5", "localhost"], &[]);
        for (gate, raw) in [
            (WebGate::AnyHost, "http://127.0.0.1/admin"),
            (WebGate::AnyHost, "http://[::1]/"),
            (allowlist_gate(&listed), "http://10.0.0.5/"),
            (allowlist_gate(&listed), "http://localhost:8080/"),
        ] {
            let url = parse_web_fetch_url(raw).unwrap();
            let err = resolve_public_web_fetch_target(&url, &gate)
                .await
                .unwrap_err();
            assert!(err.contains("private/local"), "{raw}: {err}");
        }
    }

    #[test]
    fn only_the_allowlist_gate_checks_hosts() {
        let policy = allowlist(&["github.com"], &[]);
        assert!(check_web_host(&allowlist_gate(&policy), "github.com").is_ok());
        assert!(check_web_host(&allowlist_gate(&policy), "example.org").is_err());
        assert!(check_web_host(&allowlist_gate(&policy), "[2606:4700::1111]").is_err());
        assert!(check_web_host(&WebGate::AnyHost, "example.org").is_ok());
        assert!(check_web_host(&WebGate::Unchecked, "example.org").is_ok());
    }

    #[test]
    fn web_search_redirects_stay_on_the_provider_over_https() {
        for (raw, allowed) in [
            ("https://html.duckduckgo.com/html/?q=x", true),
            ("https://duckduckgo.com/", true),
            ("http://html.duckduckgo.com/html/", false),
            ("https://duckduckgo.com.evil.org/", false),
            ("https://evilduckduckgo.com/", false),
            ("https://example.org/", false),
        ] {
            let url = reqwest::Url::parse(raw).unwrap();
            assert_eq!(provider_redirect_allowed(&url), allowed, "{raw}");
        }
    }

    // ------------------------------------------------------------------
    // Network mode in bash_exec
    // ------------------------------------------------------------------

    fn network(mode: NetworkMode) -> NetworkPolicy {
        NetworkPolicy {
            mode,
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
        }
    }

    #[test]
    fn network_modes_map_onto_the_sandbox_network_per_platform() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        use NetworkMode::{Allowlist, HostNetwork, Off};
        use SandboxNetworkMode::{Disabled, Host};
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        let macos = status_for(Some(SandboxBackend::Seatbelt));
        // Without a sandbox nothing is enforced, so nothing is refused (D17).
        let none = status_for(None);
        for (status, mode, expected) in [
            (&linux, Off, Ok(Disabled)),
            (&linux, Allowlist, Ok(Host)),
            (&linux, HostNetwork, Ok(Host)),
            (&macos, Off, Ok(Disabled)),
            (&macos, Allowlist, Err(())),
            (&macos, HostNetwork, Ok(Host)),
            (&none, Off, Ok(Disabled)),
            (&none, Allowlist, Ok(Host)),
            (&none, HostNetwork, Ok(Host)),
        ] {
            assert_eq!(
                sandbox_network(mode, status).map_err(|_| ()),
                expected,
                "{:?} {mode:?}",
                status.backend
            );
        }
        assert_eq!(
            sandbox_network(Allowlist, &macos).unwrap_err(),
            "Allowlist network mode: Not available on macOS yet. Choose Off or Host network \
             in the agent's Access settings."
        );
    }

    #[test]
    fn the_shell_profile_takes_its_network_from_the_mode() {
        use crate::assistant::sandbox::{status_for, SandboxBackend};
        let workspace = tempdir().unwrap();
        let profile = |mode, status| {
            let mut context = context_with_grants(workspace.path(), Vec::new());
            let mut execution = context.run_context.execution.clone();
            execution.sandbox.network_access = network(mode);
            context.run_context = std::sync::Arc::new(RunContext {
                execution,
                ..(*context.run_context).clone()
            });
            let filesystem = filesystem_policy(&context).unwrap();
            sandbox_profile(&context, workspace.path().to_path_buf(), filesystem, status)
        };
        let linux = status_for(Some(SandboxBackend::Bubblewrap));
        assert_eq!(
            profile(NetworkMode::Off, &linux).unwrap().network,
            SandboxNetworkMode::Disabled
        );
        let macos = status_for(Some(SandboxBackend::Seatbelt));
        assert!(profile(NetworkMode::Allowlist, &macos).is_err());
    }

    // ------------------------------------------------------------------
    // evaluate_command_policy — the ask-first list
    // ------------------------------------------------------------------

    use crate::config::types::ShellCapabilityConfig;
    use crate::config::{ExecutionCapabilityConfig, ShellAccessMode};

    fn config_in(mode: ShellAccessMode, rules: &[&str]) -> ExecutionCapabilityConfig {
        ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode,
                ask_first_command_prefixes: rules.iter().map(|s| s.to_string()).collect(),
            },
            ..Default::default()
        }
    }

    /// A Restricted (Ask first) agent whose ask-first list is `rules`.
    fn ask_first_config(rules: &[&str]) -> ExecutionCapabilityConfig {
        config_in(ShellAccessMode::Restricted, rules)
    }

    /// A Full agent with `rules` on its ask-first list, which Full never reads.
    fn full_execution_config(rules: &[&str]) -> ExecutionCapabilityConfig {
        config_in(ShellAccessMode::Full, rules)
    }

    /// The rows `command` asks about; empty when it runs without asking.
    fn asked(exec: &ExecutionCapabilityConfig, command: &str) -> Vec<AskFirstRow> {
        match evaluate_command_policy(exec, command, &[]) {
            PolicyResult::AskFirst(rows) => rows,
            PolicyResult::Allow => Vec::new(),
        }
    }

    fn asks_first(exec: &ExecutionCapabilityConfig, command: &str) -> bool {
        !asked(exec, command).is_empty()
    }

    #[test]
    fn ask_first_reads_every_command_of_the_input() {
        // The matcher's readings of each row, each run against a rule on the
        // ask-first list.
        let cases: &[(&str, &str)] = &[
            ("git push", "git push"),
            ("git push", "git push --force origin main"),
            ("git push", "cd x && git push"),
            ("git push", "git log | git push"),
            ("git push", "timeout 60 git push origin main"),
            ("git push", "git -C ../r push"),
            ("git push", "git -c x=y push"),
            ("git push", "git --no-pager -C repo push"),
            ("git push", "FOO=1 /usr/bin/git push"),
            ("git push", "printf x | xargs git push"),
            ("git push", "if true; then git push; fi"),
            ("git push", "echo $(git push)"),
            ("git push", "cat <<EOF\n$(git push)\nEOF"),
            ("git push", "git -c diff.external=/bin/echo push"),
            ("git push", "git -C repo -c e=v push"),
            ("git status", "git --git-dir=.git status --short"),
            ("git status", "git --git-dir .git status --short"),
            ("git status", "git status -C repo"),
            ("git status", "git --config-env core.fsmonitor=FSMON status"),
            ("git status", "timeout 600 git -C repo status --short"),
            ("git status", "FOO=bar git -C repo status --short"),
            ("git -c", "FOO=bar git -c diff.external=/bin/echo diff"),
            ("rm", "rm -rf /tmp"),
            ("rm", "timeout 5 rm x"),
            ("rm", "xargs rm"),
            ("rm", "find . | xargs rm -f"),
            ("rm", "find . -exec rm {} +"),
            ("rm", "find . -name x -execdir rm -f {} \\;"),
            ("rm", "find . -ok /bin/rm {} ';'"),
            ("git push", "find . -exec git -C r push \\;"),
            ("rm", "FOO=bar rm -rf /tmp/x"),
            ("rm", "/usr/bin/rm -rf x"),
            ("rm", "./rm x"),
            ("rm", "FOO=1 /bin/rm x"),
            ("rm", "ls; ../bin/rm x"),
            ("rm", "FOO='a b' /usr/bin/rm x"),
            ("rm", "rm\tx"),
            ("rm", "/bin/rm\t-rf x"),
            ("rm", "A=$'\\'' rm x"),
            ("rm", "FOO=$'a\\' b' /usr/bin/rm x/y"),
            ("rm", "A+=x A[1 + 1]=y rm x"),
            ("rm", "timeout 60 rm -rf x"),
            ("rm", "timeout --signal=KILL 60 rm -rf x"),
            ("rm", "nice -n 5 rm -rf x"),
            ("rm", "env FOO=1 rm -rf x"),
            ("timeout", "timeout 60 cargo build"),
            ("[", "[ -f /tmp/x ]"),
        ];
        for (rule, command) in cases {
            let rules = asked(&ask_first_config(&[rule]), command)
                .into_iter()
                .map(|row| row.rule)
                .collect::<Vec<_>>();
            assert!(
                rules.iter().any(|matched| matched == rule),
                "`{command}` must ask for `{rule}`, asked for {rules:?}"
            );
        }
    }

    #[test]
    fn ask_first_does_not_fire_for_what_is_not_the_command() {
        let exec = ask_first_config(&["git push", "rm"]);
        for command in [
            "echo git push",
            "git pushes",
            "git status",
            "cat <<'EOF'\ngit push\nEOF",
            "rmdir x",
            "bash -c \"git push\"",
            "gh api repos/o/r -X DELETE",
            "find . -name x",
            "find . -name rm -print",
            "find . -exec ls {} +",
        ] {
            assert!(asked(&exec, command).is_empty(), "`{command}` must run");
        }
    }

    #[test]
    fn two_matches_in_one_input_give_one_request_with_two_rows() {
        let exec = ask_first_config(&["git push", "rm"]);
        assert_eq!(
            asked(&exec, "rm -f x && cargo test && git push origin main"),
            vec![
                AskFirstRow {
                    text: "rm -f x".to_string(),
                    rule: "rm".to_string(),
                },
                AskFirstRow {
                    text: "git push origin main".to_string(),
                    rule: "git push".to_string(),
                },
            ]
        );
    }

    #[test]
    fn the_default_list_asks_in_restricted_and_never_in_full() {
        let mut restricted = ask_first_config(&[]);
        restricted.shell.ask_first_command_prefixes =
            crate::config::types::default_ask_first_command_prefixes();
        let mut full = restricted.clone();
        full.shell.mode = ShellAccessMode::Full;
        for command in [
            "git push",
            "cd x && git push",
            "timeout 5 rm x",
            "rm -rf x",
            "printf x | xargs rm",
            "sudo ls",
            "cargo publish",
            "gh api -X DELETE repos/o/r",
        ] {
            assert!(asks_first(&restricted, command), "`{command}` must ask");
            assert!(
                matches!(
                    evaluate_command_policy(&full, command, &[]),
                    PolicyResult::Allow
                ),
                "`{command}` must run without a card in Full"
            );
        }
        // A former Restricted agent runs everyday commands without a card.
        for command in ["cargo test", "ls -la", "git status", "touch x && cat x"] {
            assert!(!asks_first(&restricted, command), "`{command}` must run");
        }
    }

    #[test]
    fn the_default_list_asks_for_common_spellings_of_its_commands() {
        let mut exec = ask_first_config(&[]);
        exec.shell.ask_first_command_prefixes =
            crate::config::types::default_ask_first_command_prefixes();
        for (command, rule) in [
            ("kubectl -n ns delete pod x", "kubectl delete"),
            ("kubectl --context prod delete pod x", "kubectl delete"),
            ("kubectl --namespace=ns delete pod x", "kubectl delete"),
            (
                "kubectl --certificate-authority ca.crt delete pod x",
                "kubectl delete",
            ),
            ("kubectl --profile cpu delete pod x", "kubectl delete"),
            ("docker --context x push img", "docker push"),
            ("docker -H tcp://x push img", "docker push"),
            ("docker image push img", "docker image push"),
            ("docker --context x image push img", "docker image push"),
            ("npm --prefix x publish", "npm publish"),
            ("npm -w pkg publish", "npm publish"),
            ("npm --tag beta publish", "npm publish"),
            ("npm --otp 123456 publish", "npm publish"),
            ("npm --access public publish", "npm publish"),
            ("npm -L project publish", "npm publish"),
            ("npm --location project publish", "npm publish"),
            ("npm --scope @o --auth-type web publish", "npm publish"),
            ("cargo +nightly publish", "cargo publish"),
            ("cargo -Z unstable publish", "cargo publish"),
            ("pnpm --filter x publish", "pnpm publish"),
            ("pnpm -C dir publish", "pnpm publish"),
            ("yarn npm publish", "yarn npm publish"),
            ("gh -R o/r release create v1", "gh release"),
            ("gh --repo o/r release delete v1", "gh release"),
            ("gh api -X DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api -XDELETE repos/o/r", "gh api -X DELETE"),
            ("gh api --method DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api --method=DELETE repos/o/r", "gh api -X DELETE"),
            ("gh api -X=DELETE repos/o/r", "gh api -X DELETE"),
            ("timeout 5 gh api --method DELETE x", "gh api -X DELETE"),
            (
                "gh api repos/o/r/git/refs/heads/x -X DELETE",
                "gh api -X DELETE",
            ),
            (
                "env -S \"gh api --method DELETE repos/o/r\"",
                "gh api -X DELETE",
            ),
            (
                "find . -exec gh api --method DELETE {} \\;",
                "gh api -X DELETE",
            ),
        ] {
            let rules: Vec<String> = asked(&exec, command)
                .into_iter()
                .map(|row| row.rule)
                .collect();
            assert_eq!(rules, [rule], "`{command}`");
        }
        for command in [
            "kubectl -n ns get pods",
            "kubectl --context prod get pods",
            "docker --context x ps",
            "docker image ls",
            "npm --prefix x run build",
            "cargo +nightly build",
            "pnpm --filter x test",
            "yarn npm info x",
            "gh -R o/r pr view 1",
            "gh api repos/o/r",
            "gh api -X GET repos/o/r",
        ] {
            assert!(!asks_first(&exec, command), "`{command}` must run");
        }
    }

    #[test]
    fn a_row_lists_every_rule_it_matches() {
        let exec = ask_first_config(&["git push", "rm", " git ", "git push"]);
        let rules: Vec<String> = asked(&exec, "git push origin")
            .into_iter()
            .map(|row| row.rule)
            .collect();
        assert_eq!(rules, ["git push", "git"]);
        // With every listed rule removed, the row runs: none is left to ask.
        assert!(matches!(
            evaluate_command_policy(&exec, "git push origin", &rules),
            PolicyResult::Allow
        ));
        assert!(asks_first(&exec, "git status"), "`git` still asks");
    }

    #[test]
    fn always_allow_stops_the_rules_asking_for_the_rest_of_this_run_only() {
        let workspace = tempdir().unwrap();
        let run = context_with_grants(workspace.path(), Vec::new());
        let other_run = context_with_grants(workspace.path(), Vec::new());
        let exec = ask_first_config(&["git push", "git"]);
        let command = "git push origin main";
        let asks_in = |context: &ToolExecutionContext| {
            matches!(
                evaluate_command_policy(&exec, command, &context.run_removed_ask_first_snapshot()),
                PolicyResult::AskFirst(_)
            )
        };
        let PolicyResult::AskFirst(rows) = evaluate_command_policy(&exec, command, &[]) else {
            panic!("`{command}` must ask");
        };
        let rules = vec!["git push".to_string(), "git".to_string()];

        apply_ask_first_decision(&run, &rows, &rules, AskFirstDecision::AllowOnce)
            .expect("Allow once runs the command");
        assert!(asks_in(&run), "Allow once keeps the rules");

        apply_ask_first_decision(&run, &rows, &rules, AskFirstDecision::AlwaysAllow)
            .expect("Always allow runs the command");
        assert!(!asks_in(&run), "the same run asks no more");
        assert!(asks_in(&other_run), "another run keeps its own snapshot");

        let denied = apply_ask_first_decision(
            &other_run,
            &rows,
            &rules,
            AskFirstDecision::Deny {
                reason: Some("no".to_string()),
            },
        );
        assert_eq!(denied, Err(ask_first_denied_message(&rows, Some("no"))));
        assert!(other_run.had_warnings() && !run.had_warnings());
    }

    #[test]
    fn a_rule_removed_in_this_run_no_longer_asks() {
        let exec = ask_first_config(&["git push", "rm"]);
        let removed = vec!["git push".to_string()];
        assert!(matches!(
            evaluate_command_policy(&exec, "git push origin main", &removed),
            PolicyResult::Allow
        ));
        // Only the removed rule: another rule still asks.
        let PolicyResult::AskFirst(rows) =
            evaluate_command_policy(&exec, "git push && rm -f x", &removed)
        else {
            panic!("rm must still ask");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule, "rm");
    }

    #[test]
    fn an_empty_ask_first_list_asks_nothing() {
        let exec = ask_first_config(&[]);
        assert!(!asks_first(&exec, "git push && rm -rf x"));
    }

    #[test]
    fn denied_message_names_each_row_its_rule_and_the_reason() {
        let rows = vec![
            AskFirstRow {
                text: "rm -f x".to_string(),
                rule: "rm".to_string(),
            },
            AskFirstRow {
                text: "git push".to_string(),
                rule: "git push".to_string(),
            },
        ];
        assert_eq!(
            ask_first_denied_message(&rows, Some("  keep x  ")),
            "User denied `rm -f x` (ask-first rule `rm`), `git push` (ask-first rule `git push`). \
             Reason: keep x."
        );
        assert_eq!(
            ask_first_denied_message(&rows[..1], Some(" ")),
            "User denied `rm -f x` (ask-first rule `rm`)."
        );
        assert_eq!(
            ask_first_denied_message(&rows[..1], None),
            "User denied `rm -f x` (ask-first rule `rm`)."
        );
    }

    #[test]
    fn ask_first_sees_commands_inside_blocks() {
        // Each of these used to reach the matcher as one segment headed by
        // grammar (`f(){`, `{`, `(`, `for`, `cat <<EOF`), so `rm` ran.
        let exec = ask_first_config(&["rm"]);
        for command in [
            "f(){ rm -rf x; }; f",
            "f() { rm -rf x; }",
            "function f { rm -rf x; }",
            "{ rm -rf x; }",
            "(rm -rf x)",
            "for i in 1; do rm -rf x; done",
            "if true; then rm -rf x; fi",
            "case a in a) rm -rf x;; esac",
            "shopt -s extglob\ncase a in @(a|b)) rm -rf y;; esac",
            "echo $(case x in x) a;; esac); rm -rf x",
            "echo $(echo case in a); rm -rf x",
            "echo $(echo case); echo $(echo in a); rm -rf x",
            "! rm -rf x",
            "cat <<EOF | rm -rf x\nhi\nEOF",
            "cat <<\\EOF\nhi\nEOF\nrm -rf x",
            "rm(){ :; }; unset -f rm; rm -rf ~",
            "case x\nin x) rm -rf y;; esac",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::AskFirst(_)
                ),
                "{command:?} should ask for rm"
            );
        }
    }

    #[test]
    fn ask_first_reads_every_row_when_one_is_asked_by_exact_text() {
        // The value arithmetic evaluates, or the arguments a call passes to
        // `"$@"`, are written in another row.
        for command in [
            "i='a[$(rm -rf x)]'; cat <<EOF\n$((i))\nEOF",
            "i='a[$(rm -rf x)]'; for ((j=0;j<i;j++)); do :; done",
            "f(){ \"$@\"; }; f rm -rf x",
            "ls\nrm -rf x <<'E'\nno end",
        ] {
            for (exec, asks) in [
                (full_execution_config(&["rm"]), false),
                (ask_first_config(&["rm"]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command:?}");
            }
        }
    }

    #[test]
    fn ask_first_word_scan_reads_paths_and_entries_of_several_words() {
        for (rule, command) in [
            ("rm", "x='a[$(/bin/rm -rf y)]'; echo $((x))"),
            ("git push", "x='a[$(git push origin)]'; echo $((x))"),
        ] {
            for (exec, asks) in [
                (full_execution_config(&[rule]), false),
                (ask_first_config(&[rule]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{rule:?} {command:?}");
            }
        }
    }

    #[test]
    fn heredoc_bash_ends_inside_a_substitution_hides_no_command() {
        // In `$(…)` and `<(…)` bash ends a heredoc at a line that starts
        // with its delimiter (`EOF)`) and runs `rm -rf x`.
        for command in [
            "echo $(cat <<EOF\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<'EOF'\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<-EOF\nhi\n\tEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "cat <(cat <<EOF\nhi\nEOF)\nrm -rf x\n: <<ls\nEOF\n)\nls",
            "echo $(cat <<EOF\nhi\nEOFX)\nrm -rf x\n: <<ls\nEOF\n)\nls",
        ] {
            for (exec, asks) in [
                (full_execution_config(&["rm"]), false),
                (ask_first_config(&["rm"]), true),
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command:?}");
            }
        }
    }

    #[test]
    fn ask_first_reads_every_word_of_unparsed_input() {
        // bash runs the `touch PWN` in each; the splitter cannot find it, so
        // the rules are read against every word of the input. A `touch`
        // rule asks in Restricted, unless this run removed it; another rule
        // does not; Full never asks.
        let commands = [
            "echo $(case a in a) echo;& b) echo;; esac); touch PWN",
            "cat <<EOF|touch PWN\nhi\nEOF",
            "echo x\r# ; touch PWN",
            "echo $(ls >& case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls &>> case in a)\ntouch PWN\n;; esac)",
            "echo $(ls >| case in a)\ntouch PWN\n;; esac)",
            "echo $(cat <& 0 case in a </dev/null)\ntouch PWN\n;; esac)",
            "echo \"$(ls >& case in a)\"\ntouch PWN\n;; esac)",
            "git --version ; for i in a; { touch PWN; }",
            "cat <<$'E'\nE\ntouch PWN",
            "cat <<$\"E\"\nE\ntouch PWN",
            "cat <<$\\\n'E'\nE\ntouch PWN",
            "cat <<$(x)\n$(x)\ntouch PWN\n$",
            "cat <<`a`\n`a`\ntouch PWN",
            "cat <<E\\\nX\nEX\\\n\ntouch PWN\nEX",
            "cat <<''\n'\n\ntouch PWN\n'",
            "cat <<\"\"\n'\n\ntouch PWN\n'",
            "cat <<'E\u{1}'\nE\u{1}\n'\nE\u{1}\u{1}\ntouch PWN\n'",
            "cat <<'E\u{7f}'\nE\u{7f}\n'\nE\u{1}\u{7f}\ntouch PWN\n'",
            "cat <<-\"\tE\"\n\tE\ntouch PWN",
            "cat <<-'\tE'\n\tE\ntouch PWN",
            "cat <<-\\\tE\n\tE\ntouch PWN",
            "cat <<\"E\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<\"E\\\"F\"\nE\"F\ntouch PWN",
            "cat <<\"E\\$\"\nE$\ntouch PWN\nE\\$",
            "cat <<\"E\\`\"\nE`\ntouch PWN\nE\\`",
            "cat <<E\"\\\\\"\nE\\\ntouch PWN\nE\\\\",
            "cat <<-\"E\\\\\"\n\tE\\\ntouch PWN\nE\\\\",
            "[ x <<\"E\\\\\" ]\nE\\\ntouch PWN\nE\\\\",
            "echo x\n((echo a) ; touch PWN )",
            "((touch PWN) )",
            "echo $((echo a) ; touch PWN )",
        ];
        for (exec, removed, asks) in [
            (ask_first_config(&["touch"]), &[][..], true),
            (
                ask_first_config(&["touch"]),
                &["touch".to_string()][..],
                false,
            ),
            (ask_first_config(&["sudo"]), &[][..], false),
            (full_execution_config(&["touch"]), &[][..], false),
        ] {
            for command in commands {
                let result = evaluate_command_policy(&exec, command, removed);
                assert_eq!(
                    matches!(result, PolicyResult::AskFirst(_)),
                    asks,
                    "{:?} {removed:?} {command:?} -> {result:?}",
                    exec.shell.mode
                );
            }
        }
    }

    #[test]
    fn ask_first_sees_a_command_after_an_escaped_blank_and_hash() {
        let exec = ask_first_config(&["touch"]);
        assert_eq!(
            asked(&exec, "echo a\\ #; touch PWN"),
            vec![AskFirstRow {
                text: "touch PWN".to_string(),
                rule: "touch".to_string(),
            }]
        );
        let continued = "echo a\\ \\\n#; touch PWN";
        // The continued form is Unparsed: the rules are read against its words.
        assert!(asks_first(&exec, continued));
        assert!(!asks_first(&full_execution_config(&["touch"]), continued));
    }

    #[test]
    fn full_mode_with_the_default_list_never_asks() {
        // Owner rule: Full mode never raises a card and refuses nothing.
        let exec = ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Full,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!exec.shell.ask_first_command_prefixes.is_empty());
        for command in [
            "cd /x && SCRATCH=a bash tools/check-links.sh 2>&1 | tail -30",
            "bash -c 'echo hi'",
            "echo \"unclosed",
            "rm -rf x",
            "timeout 5 rm x",
            "git push --force",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} must run without a card in Full mode"
            );
        }
    }

    #[test]
    fn full_mode_heredoc_commands_run_without_a_prompt() {
        // Hotfix regression cases: heredoc-fed scripts must run in Full
        // mode without an approval prompt while the default ask-first list is
        // active. The quoted-delimiter form is Opaque, the unquoted one
        // also yields the command inside its `$(...)`, and the
        // write-then-run form is Unparsed — none of them may prompt.
        let exec = ExecutionCapabilityConfig {
            shell: ShellCapabilityConfig {
                mode: ShellAccessMode::Full,
                ..Default::default()
            },
            ..Default::default()
        };
        let commands = [
            "python3 - <<'EOF'\nimport re, pathlib\np = pathlib.Path(\"x.py\"); s = p.read_text()\nprint(f\"{len(s)} $HOME `x`\")\nEOF",
            "python3 - <<EOF\nimport os\nprint(\"x $(touch PWN)\")\nEOF",
            "cat > f.py <<'EOF'\nprint(1)\nEOF && python3 f.py",
        ];
        for command in commands {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} must run without a prompt in Full mode"
            );
        }
    }

    #[test]
    fn heredoc_forms_current_segment_kinds() {
        // Pins what the splitter currently makes of each heredoc form.
        // The Full-mode no-prompt rule must hold whichever kind appears.
        use crate::assistant::tools::command_splitter::{split_command, Segment};
        let kinds = |input: &str| -> Vec<&'static str> {
            split_command(input)
                .iter()
                .map(|segment| match segment {
                    Segment::Simple(_) => "Simple",
                    Segment::Opaque(_) => "Opaque",
                    Segment::Unparsed(_) => "Unparsed",
                    Segment::Exact(_) => "Exact",
                })
                .collect()
        };
        assert_eq!(
            kinds("python3 - <<'EOF'\nprint(\"$HOME `x`\")\nEOF"),
            ["Opaque"]
        );
        // An unquoted delimiter expands its body, so the splitter reports
        // the command inside the `$(...)` as a segment of its own — the
        // ask-first list still applies to it.
        assert_eq!(
            kinds("python3 - <<EOF\nprint(\"x $(touch PWN)\")\nEOF"),
            ["Opaque", "Simple"]
        );
        assert_eq!(
            kinds("cat > f.py <<'EOF'\nprint(1)\nEOF && python3 f.py"),
            ["Unparsed"]
        );
    }

    #[test]
    fn augment_timeout_error_passes_through_non_timeout() {
        let err = "Shell command failed: boom".to_string();
        assert_eq!(
            augment_timeout_error(err.clone(), DEFAULT_BASH_TIMEOUT_MS, false),
            err
        );
    }

    #[test]
    fn augment_timeout_error_points_at_timeoutms_below_cap() {
        let err = "Sandboxed shell command timed out after 300000 ms".to_string();
        let out = augment_timeout_error(err, DEFAULT_BASH_TIMEOUT_MS, false);
        assert!(out.contains("timed out after 300000 ms"));
        assert!(out.contains("default timeout"));
        assert!(out.contains("`timeoutMs`"));
        assert!(out.contains(&MAX_BASH_TIMEOUT_MS.to_string()));
    }

    #[test]
    fn augment_timeout_error_labels_requested_timeout() {
        let err = "Sandboxed shell command timed out after 600000 ms".to_string();
        let out = augment_timeout_error(err, 600_000, true);
        assert!(out.contains("requested timeout"));
    }

    #[test]
    fn augment_timeout_error_at_cap_advises_narrowing() {
        let err = "Sandboxed shell command timed out after 1800000 ms".to_string();
        let out = augment_timeout_error(err, MAX_BASH_TIMEOUT_MS, true);
        assert!(out.contains("maximum `timeoutMs`"));
        assert!(out.contains("narrow the command"));
    }

    // ------------------------------------------------------------------
    // Commands that run their arguments, heredoc bodies, lookups
    // ------------------------------------------------------------------

    #[test]
    fn ask_first_sees_the_command_an_argument_runner_runs() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "find . | xargs rm -f",
                "find . | xargs -0 -I {} -n 1 rm -f {}",
                "exec rm -rf x",
                "exec -a name rm x",
                "eval rm -rf x",
                "eval -- timeout 5 rm x",
                "watch rm x",
                "watch -d -n 2 rm x",
                "nohup xargs -P 4 rm",
                "xargs eval rm",
                "xargs rm",
                "timeout 5 rm x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn argument_runners_without_a_listed_command_run() {
        let exec = full_execution_config(&["rm"]);
        for command in ["exec >log 2>&1", "xargs grep foo", "watch -n 2 ls"] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command}"
            );
        }
        let exec = ask_first_config(&["rm"]);
        for command in ["exec >log 2>&1", "xargs grep foo", "watch -n 2 ls"] {
            assert!(!asks_first(&exec, command), "{command}");
        }
    }

    #[test]
    fn ask_first_sees_commands_an_unquoted_heredoc_body_runs() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "cat <<EOF\n${x:-$(rm -rf ~)}\nEOF",
                "cat <<EOF\n`rm -rf ~`\nEOF",
                "cat <<EOF\nx\n`rm b` $(echo c)\nEOF",
                "cat <<EOF\n$(echo `rm a`)\nEOF",
                "cat <<EOF\n`echo \\`rm a\\``\nEOF",
            ] {
                assert_eq!(
                    asks_first(&exec, command),
                    asks,
                    "{command:?} must ask first"
                );
            }
        }
    }

    #[test]
    fn quoted_heredoc_body_runs_nothing_ask_first_sees() {
        let command = "cat <<'EOF'\n`rm x` $(rm y)\nEOF";
        assert!(!asks_first(&full_execution_config(&["rm"]), command));
        assert!(!asks_first(&ask_first_config(&["rm"]), command));
    }

    #[test]
    fn command_lookup_is_not_the_command_it_names() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in ["command -v rm", "command -V rm", "command -pv rm"] {
                assert!(!asks_first(&exec, command), "{command} only looks rm up");
            }
            for command in ["command rm x", "command -p rm x"] {
                assert_eq!(asks_first(&exec, command), asks, "{command} runs rm");
            }
        }
    }

    #[test]
    fn runner_options_are_read_as_getopt_reads_them() {
        // `xargs -0I grep echo RAN` runs `echo` with `grep` as the replace
        // string, `exec -la grep echo RAN` runs `echo` named `grep`, and
        // `xargs --process grep` abbreviates `--process-slot-var grep`.
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "xargs -0n 1 rm -rf x",
                "xargs -0I grep rm -rf x",
                "xargs -rI grep rm -rf x",
                "xargs -J % rm -rf x",
                "xargs --process grep rm -rf x",
                "xargs -in rm -rf x",
                "exec -la grep rm -rf x",
                "env -iu FOO rm -rf x",
                "env - rm -rf x",
                "timeout -sKILL 5 rm -rf x",
                "timeout --pres 5 rm -rf x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn ask_first_sees_a_command_under_every_reading_of_runner_options() {
        for (exec, asks) in [
            (full_execution_config(&["rm"]), false),
            (ask_first_config(&["rm"]), true),
        ] {
            for command in [
                "xargs --max-lines rm -rf x",
                "xargs --max-l rm -rf x",
                "xargs -0I rm echo x",
                "xargs -I rm rm x",
                "xargs -Irm rm",
                "xargs --replace rm",
                "xargs --replace=rm rm x",
                "xargs --frob rm echo x",
                "xargs --frob x rm -rf y",
                "exec xargs -0I rm echo x",
                "env -S'rm -rf' x",
                "env -Srm -rf x",
                "env -S 'rm -rf' x",
                "env \"-Srm -rf\" x",
                "env --split-string='rm -rf' x",
                "env -i -S'timeout 5 rm -rf' x",
                "env -S'\\_rm -rf' x",
                "env -S'env -S\\_rm' x",
                "env -S'r''m -rf' x",
                "taskset 1 rm -rf x",
                "taskset 0x3 rm -rf x",
                "taskset -c 0,1 rm x",
                "chrt 10 rm -rf x",
                "chrt -f 10 rm -rf x",
                "chrt -o rm x",
                "chrt 10 -- rm x",
            ] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
        // Only a word an option may take is read as the command.
        let exec = ask_first_config(&["rm"]);
        for command in [
            "xargs -0 git rm x",
            "xargs -n 1 git rm",
            "xargs grep rm",
            "env -u FOO git rm",
            "timeout 5 git rm",
            "chrt -o echo rm",
            "taskset 1 git rm x",
        ] {
            assert!(
                matches!(
                    evaluate_command_policy(&exec, command, &[]),
                    PolicyResult::Allow
                ),
                "{command} runs no rm"
            );
        }
    }

    #[test]
    fn ask_first_sees_a_listed_subcommand_under_every_reading() {
        for (exec, asks) in [
            (full_execution_config(&["git push"]), false),
            (ask_first_config(&["git push"]), true),
        ] {
            for command in ["xargs -0I git push origin", "env -S'\\_git push'"] {
                assert_eq!(asks_first(&exec, command), asks, "{command} must ask first");
            }
        }
    }

    #[test]
    fn many_replace_strings_take_linear_time() {
        let command = format!("xargs {}git {}", "-I zq ".repeat(5_000), "w ".repeat(5_000));
        let started = std::time::Instant::now();
        for exec in [full_execution_config(&["rm"]), ask_first_config(&["rm"])] {
            evaluate_command_policy(&exec, &command, &[]);
        }
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn deeply_nested_runners_do_not_overflow_the_stack() {
        let handle = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(|| {
                for runner in ["exec ", "nohup "] {
                    let command = format!("{}grep x", runner.repeat(20_000));
                    assert!(
                        !asks_first(&ask_first_config(&["rm"]), &command),
                        "{runner}"
                    );
                }
                let command = format!("{}rm x", "exec ".repeat(20_000));
                assert!(asks_first(&ask_first_config(&["rm"]), &command));
            })
            .expect("spawn");
        handle.join().expect("nested runners must not overflow");
    }
}
