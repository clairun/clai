//! Ask-first approval flow.
//!
//! When a Restricted agent runs a command that matches a rule on its
//! ask-first list, the bash tool, instead of running it:
//!
//! 1. Builds a [`PermissionRequest`] listing each row of the command that
//!    matched, with the rule it matched.
//! 2. Registers a [`oneshot::Sender`] in [`PendingApprovals`] keyed by a
//!    fresh request id.
//! 3. Emits [`PERMISSION_REQUEST_EVENT`] to the frontend and emits
//!    [`PERMISSION_ATTENTION_EVENT`] with the new per-workspace count so
//!    the fleet card can render a badge.
//! 4. `.await`s the oneshot until the user decides or the run is cancelled.
//! 5. When the frontend invokes [`submit_permission_decision`], the
//!    backend looks up the sender and, for "Always allow", removes the
//!    matched rules from the agent's ask-first list *before* delivering the
//!    decision through the oneshot. The bash tool resumes and either runs
//!    the command or returns an error carrying the user's reason. If that
//!    save fails, the request stays pending and the card shows the error.
//!
//! Sequencing: persistence happens *before* the oneshot delivery so that a
//! crash between user click and command execution still leaves the removal
//! on disk.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::config::workspace_config;
use crate::AppState;

pub const PERMISSION_REQUEST_EVENT: &str = "permissions://request";
pub const PERMISSION_ATTENTION_EVENT: &str = "permissions://attention";
/// Emitted whenever a pending request leaves the queue: a user decision,
/// a cancelled or ended run (reaping a wait orphaned by a CLI transport
/// drop), or a re-asked command superseding the stale request. Every view
/// showing the request drops it on this.
pub const PERMISSION_RESOLVED_EVENT: &str = "permissions://resolved";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum PermissionScope {
    /// Persist on the agent. Where that lands depends on which agent: the
    /// workspace's Main keeps it in its own config, while a shared teammate
    /// keeps it on its library definition, so the allowance travels with the
    /// agent to every workspace that assigned it. `PermissionRequest`
    /// carries the destination so the prompt can say which one is happening.
    Agent,
    // Workspace,  // deferred: requires workspace_id plumbing through
    //             // AgentConfig / SessionContext (runner.rs currently
    //             // populates workspace_id with the agent's own id).
}

/// A row of a command that matched an ask-first rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AskFirstRow {
    pub text: String,
    /// The rule on the agent's ask-first list that the row matched.
    pub rule: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct PermissionRequest {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub command: String,
    pub rows: Vec<AskFirstRow>,
    /// True when "Always allow" edits a shared agent definition rather than
    /// this workspace's Main.
    ///
    /// Removing a rule stops it asking wherever the list lives, so where that
    /// is has to be visible while the user is deciding: for a shared teammate
    /// it is the definition, in force wherever that agent works, including
    /// workspaces that assign it later.
    #[serde(default)]
    pub persists_to_shared_agent: bool,
    /// The other workspaces that decision reaches today. Empty when the agent
    /// is this workspace's Main, or when nobody else has the teammate on their
    /// team yet.
    #[serde(default)]
    pub also_affects_workspaces: Vec<String>,
}

impl PermissionRequest {
    /// Build the request an agent waits on, resolving where "Always allow"
    /// would be saved.
    ///
    /// The destination is derived here rather than passed in, so no call site
    /// can ask for consent without stating its scope.
    pub fn for_agent(
        state: &AppState,
        workspace_id: Option<String>,
        agent_id: Option<String>,
        command: String,
        rows: Vec<AskFirstRow>,
    ) -> Self {
        let reach = match (workspace_id.as_deref(), agent_id.as_deref()) {
            (Some(workspace), Some(agent)) => shared_allowance_reach(state, workspace, agent),
            _ => None,
        };
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id,
            agent_id,
            // Agent display name isn't on the runtime context; the frontend
            // resolves it from agent_id via existing workspace queries.
            agent_name: None,
            command,
            rows,
            persists_to_shared_agent: reach.is_some(),
            also_affects_workspaces: reach.unwrap_or_default(),
        }
    }

    /// The distinct rules the request's rows matched, in order.
    pub fn rules(&self) -> Vec<String> {
        let mut rules: Vec<String> = Vec::new();
        for row in &self.rows {
            if !rules.contains(&row.rule) {
                rules.push(row.rule.clone());
            }
        }
        rules
    }
}

/// Where an "always" decision for this agent is saved, and what else it reaches.
///
/// `None` means the workspace's own Main: its decisions stay local. `Some(list)`
/// means a shared definition, with the list naming the *other* workspaces that
/// have the same teammate on their team right now — possibly empty, which is
/// still a shared save.
pub fn shared_allowance_reach(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
) -> Option<Vec<String>> {
    let root = state.workspace_root(workspace_id)?;
    let workspace = workspace_config::load(&root).ok()?;
    let definition_id = workspace.assignment(agent_id)?.agent_definition_id.clone();
    // Copy the locators out before touching the disk: holding the index lock
    // across a read of every workspace config blocks unrelated work for as long
    // as that takes.
    let locators = match state.workspace_index.read() {
        Ok(index) => index.locators_sorted(),
        Err(_) => return Some(Vec::new()),
    };
    Some(
        locators
            .iter()
            .filter(|locator| locator.id != workspace_id)
            .filter_map(|locator| workspace_config::load(&locator.root_path).ok())
            .filter(|other| {
                other
                    .assignments
                    .iter()
                    .any(|assignment| assignment.agent_definition_id == definition_id)
            })
            .map(|other| other.title)
            .collect(),
    )
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AttentionUpdate {
    pub workspace_id: Option<String>,
    pub pending_count: u32,
}

/// The user's answer to an ask-first request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum AskFirstDecision {
    /// Run this command only.
    AllowOnce,
    /// Remove the matched rules from the agent's ask-first list, then run it.
    AlwaysAllow,
    /// Do not run it; the reason goes back to the agent in the tool error.
    Deny {
        #[serde(default)]
        #[ts(optional)]
        reason: Option<String>,
    },
}

/// In-memory registry of in-flight approval requests. Each entry holds a
/// oneshot sender used to deliver the user's decisions to the awaiting
/// bash tool.
pub struct PendingApprovals {
    inner: AsyncMutex<PendingInner>,
}

struct PendingInner {
    entries: HashMap<String, PendingEntry>,
    counts: HashMap<Option<String>, u32>,
}

#[derive(Debug)]
pub enum PendingApprovalOutcome {
    Decision(AskFirstDecision),
    Superseded,
}

pub struct PendingEntry {
    pub sender: oneshot::Sender<PendingApprovalOutcome>,
    pub workspace_id: Option<String>,
    /// The run that is awaiting this decision. Used by
    /// [`PendingApprovals::take_superseded`] so a re-asked command (after
    /// a CLI transport drop orphaned the original request) replaces the
    /// stale entry instead of stacking a duplicate card.
    pub run_id: String,
    /// The original request payload as emitted to the frontend. Stored
    /// so that components that mount after the event fired can still
    /// discover the request via [`list_pending_permission_requests`].
    pub request: PermissionRequest,
}

pub struct SupersededApproval {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub remaining: u32,
}

impl PendingApprovals {
    pub fn new() -> Self {
        Self {
            inner: AsyncMutex::new(PendingInner {
                entries: HashMap::new(),
                counts: HashMap::new(),
            }),
        }
    }

    /// Registers a new pending approval. The caller supplies the full
    /// [`PermissionRequest`] payload (including a freshly-generated
    /// `request_id` and the matched rows), which is also stored so that
    /// late-mounting frontend components can re-discover it via
    /// [`list_pending_permission_requests`]. Returns a receiver for
    /// the bash handler to `.await` and the new pending-count for the
    /// workspace (for the attention badge).
    pub async fn register(
        &self,
        request: PermissionRequest,
        run_id: String,
    ) -> (oneshot::Receiver<PendingApprovalOutcome>, u32) {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.inner.lock().await;
        let request_id = request.request_id.clone();
        let workspace_id = request.workspace_id.clone();
        inner.entries.insert(
            request_id,
            PendingEntry {
                sender: tx,
                workspace_id: workspace_id.clone(),
                run_id,
                request,
            },
        );
        let entry = inner.counts.entry(workspace_id).or_insert(0);
        *entry += 1;
        let count = *entry;
        (rx, count)
    }

    /// Returns clones of all currently-pending requests for a given
    /// workspace. Used to seed the inline approval card when a
    /// conversation view mounts after the original event fired.
    pub async fn list_for_workspace(&self, workspace_id: &str) -> Vec<PermissionRequest> {
        let inner = self.inner.lock().await;
        inner
            .entries
            .values()
            .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// Returns the pending-count per workspace id. Workspace-less requests
    /// (the `None` bucket) are dropped since the consumer keys by id.
    pub async fn counts_snapshot(&self) -> HashMap<String, u32> {
        let inner = self.inner.lock().await;
        inner
            .counts
            .iter()
            .filter_map(|(workspace, count)| workspace.as_ref().map(|id| (id.clone(), *count)))
            .collect()
    }

    /// Drops every pending entry belonging to `workspace_id` and clears
    /// its count. Used by `workspace_delete` so requests scoped to a
    /// just-deleted workspace don't linger in memory until restart.
    /// Cancels the runs awaiting those entries before dropping their
    /// senders, then returns the cancelled run ids. Channel closure is
    /// reserved for cancellation/teardown; supersede is delivered as an
    /// explicit [`PendingApprovalOutcome::Superseded`].
    pub async fn purge_workspace_canceling_runs<F>(
        &self,
        workspace_id: &str,
        mut cancel_run: F,
    ) -> Vec<String>
    where
        F: FnMut(&str),
    {
        let mut inner = self.inner.lock().await;
        let to_remove: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut run_ids = Vec::with_capacity(to_remove.len());
        for id in &to_remove {
            if let Some(entry) = inner.entries.get(id) {
                run_ids.push(entry.run_id.clone());
            }
        }
        run_ids.sort();
        run_ids.dedup();
        for run_id in &run_ids {
            cancel_run(run_id);
        }
        for id in to_remove {
            inner.entries.remove(&id);
        }
        inner.counts.remove(&Some(workspace_id.to_string()));
        run_ids
    }

    /// Removes the pending entry and decrements its workspace count.
    /// Returns the entry and the post-decrement workspace count (for
    /// emitting attention).
    pub async fn take(&self, request_id: &str) -> Option<(PendingEntry, u32)> {
        let mut inner = self.inner.lock().await;
        let entry = inner.entries.remove(request_id)?;
        let count = match inner.counts.get_mut(&entry.workspace_id) {
            Some(n) if *n > 0 => {
                *n -= 1;
                let v = *n;
                if v == 0 {
                    inner.counts.remove(&entry.workspace_id);
                }
                v
            }
            _ => 0,
        };
        Some((entry, count))
    }

    /// Puts back an entry [`take`](Self::take) removed, unless the tool
    /// stopped waiting on it meanwhile. Returns the workspace pending count
    /// once the entry is pending again, or none when it is not.
    pub async fn restore(&self, entry: PendingEntry) -> Option<u32> {
        let mut inner = self.inner.lock().await;
        // Checked under the lock: a waiter dropped later reaps the entry
        // through `take` once this returns.
        if entry.sender.is_closed() {
            return None;
        }
        let count = inner.counts.entry(entry.workspace_id.clone()).or_insert(0);
        *count += 1;
        let count = *count;
        inner
            .entries
            .insert(entry.request.request_id.clone(), entry);
        Some(count)
    }

    /// Removes every pending entry for the same run + command and returns
    /// each with the post-removal workspace count. Called by the bash
    /// approval flow right before registering a fresh request: when a CLI
    /// transport drop orphans an in-flight approval and the model re-asks
    /// the same command, the stale entry (and its UI card) is replaced by
    /// the fresh one instead of lingering next to it. The stale waiter is
    /// woken with an explicit supersede outcome, not by dropping its
    /// channel, so unrelated teardown cannot masquerade as supersede.
    pub async fn take_superseded(&self, run_id: &str, command: &str) -> Vec<SupersededApproval> {
        let mut inner = self.inner.lock().await;
        let ids: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.run_id == run_id && entry.request.command == command)
            .map(|(id, _)| id.clone())
            .collect();
        let mut taken = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(entry) = inner.entries.remove(&id) else {
                continue;
            };
            let request_id = entry.request.request_id.clone();
            let workspace_id = entry.workspace_id.clone();
            let count = match inner.counts.get_mut(&entry.workspace_id) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    let v = *n;
                    if v == 0 {
                        inner.counts.remove(&entry.workspace_id);
                    }
                    v
                }
                _ => 0,
            };
            let _ = entry.sender.send(PendingApprovalOutcome::Superseded);
            taken.push(SupersededApproval {
                request_id,
                workspace_id,
                remaining: count,
            });
        }
        taken
    }
}

impl Default for PendingApprovals {
    fn default() -> Self {
        Self::new()
    }
}

/// Tauri command — returns the list of pending permission requests for
/// a workspace. The inline approval card calls this on mount so it can
/// surface requests that were registered before it subscribed to the
/// event stream (e.g., the user navigated to the workspace after the
/// request fired).
#[tauri::command]
pub async fn list_pending_permission_requests(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<PermissionRequest>, String> {
    Ok(state
        .pending_approvals
        .list_for_workspace(&workspace_id)
        .await)
}

/// Tauri command — returns the current pending-approval count per
/// workspace. Fleet and any global attention listener call this on mount
/// so badges reflect requests that fired before the subscription was
/// established (e.g., the user was on another page when the agent
/// requested approval).
#[tauri::command]
pub async fn list_pending_permission_counts(
    state: State<'_, AppState>,
) -> Result<HashMap<String, u32>, String> {
    Ok(state.pending_approvals.counts_snapshot().await)
}

/// Tauri command — invoked by the approval card when the user decides.
/// "Always allow" removes the matched rules from the agent's ask-first list
/// *before* the decision is delivered to the awaiting bash tool, so the
/// removal survives a crash between click and command execution.
#[tauri::command]
pub async fn submit_permission_decision(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    request_id: String,
    mut decision: AskFirstDecision,
) -> Result<(), String> {
    normalize_denial_reason(&mut decision)?;
    resolve_pending_approval(
        state.inner(),
        &request_id,
        decision,
        |workspace_id, remaining| {
            emit_permission_resolved(&app, &request_id);
            emit_attention(&app, workspace_id, remaining);
        },
        |request, pending| {
            if let Err(e) = app.emit(PERMISSION_REQUEST_EVENT, request) {
                tracing::warn!("Failed to emit permission request event: {}", e);
            }
            emit_attention(&app, request.workspace_id.clone(), pending);
        },
    )
    .await
}

/// Delivers `decision` to the tool waiting on request `request_id`, saving
/// an "Always allow" first. `left_queue` gets the request's workspace and
/// that workspace's pending count once the request has left the queue.
///
/// When the save fails, the request goes back on the queue and the error
/// is returned: its card stays open with the error, the command keeps
/// waiting, and the user can answer again. `back_on_queue` then gets the
/// request and its workspace's pending count, so a view that listed the
/// queue during the save can show the request again.
async fn resolve_pending_approval(
    state: &AppState,
    request_id: &str,
    decision: AskFirstDecision,
    left_queue: impl FnOnce(Option<String>, u32),
    back_on_queue: impl FnOnce(&PermissionRequest, u32),
) -> Result<(), String> {
    let Some((entry, remaining)) = state.pending_approvals.take(request_id).await else {
        return Err(format!(
            "No pending approval with request_id `{}` (already resolved or cleared)",
            request_id
        ));
    };
    // Missing identity leaves "Always allow" scoped to this run only.
    if let (AskFirstDecision::AlwaysAllow, Some(workspace_id), Some(agent_id)) = (
        &decision,
        entry.workspace_id.as_deref(),
        entry.request.agent_id.as_deref(),
    ) {
        if let Err(error) =
            persist_always_allow(state, workspace_id, agent_id, &entry.request.rules())
        {
            tracing::warn!(
                "Could not save Always allow for `{}`: {error}",
                entry.request.command
            );
            let request = entry.request.clone();
            match state.pending_approvals.restore(entry).await {
                Some(pending) => back_on_queue(&request, pending),
                None => left_queue(request.workspace_id, remaining),
            }
            return Err(format!("Could not save Always allow: {error}"));
        }
    }
    left_queue(entry.workspace_id.clone(), remaining);
    let _ = entry
        .sender
        .send(PendingApprovalOutcome::Decision(decision));
    Ok(())
}

fn normalize_denial_reason(decision: &mut AskFirstDecision) -> Result<(), String> {
    let AskFirstDecision::Deny {
        reason: Some(value),
    } = decision
    else {
        return Ok(());
    };
    let reason = normalize_reason(value)?;
    *decision = AskFirstDecision::Deny { reason };
    Ok(())
}

/// A denial reason as the agent receives it: trimmed, none when empty, at
/// most 500 characters.
pub(crate) fn normalize_reason(value: &str) -> Result<Option<String>, String> {
    let trimmed = value.trim();
    if trimmed.chars().count() > 500 {
        return Err("Denial reason must be at most 500 characters".to_string());
    }
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// Removes `rules` from the agent's ask-first list, where the agent's
/// settings live.
///
/// The read-modify-write runs inside [`AppState::update_workspace_config`]
/// (which serializes writers behind a process-wide lock), so a concurrent
/// config write — starring a workspace, re-anchoring a schedule — cannot be
/// clobbered by this save.
pub fn persist_always_allow(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    rules: &[String],
) -> Result<(), String> {
    if rules.is_empty() {
        return Ok(());
    }

    // A teammate's ask-first list belongs to its shared definition: the user
    // is saying "this reviewer may push", not "…but only in this project".
    // Path grants go the other way (see `path_grants::persist_grant_to_agent`)
    // because a path is about this machine and this project, not about the
    // teammate.
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let workspace = workspace_config::load(&root).map_err(|e| e.to_string())?;
    if let Some(assignment) = workspace.assignment(agent_id) {
        return crate::commands::global_agents::update_definition(
            state,
            &assignment.agent_definition_id,
            |definition| {
                if remove_ask_first_rules(&mut definition.behavior, rules) {
                    definition.revision += 1;
                    definition.behavior.updated_at = chrono::Utc::now().timestamp_millis();
                }
            },
        );
    }

    // The Main has no shared definition, so its list stays in the workspace.
    state.update_workspace_config(workspace_id, |config| {
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == agent_id)
        else {
            return Err(format!("Workspace agent not found: {}", agent_id));
        };
        if remove_ask_first_rules(agent, rules) {
            let now = chrono::Utc::now().timestamp_millis();
            agent.touch(now);
            config.updated_at = now;
        }
        Ok(())
    })?;
    Ok(())
}

/// Removes every entry of the agent's ask-first list that is one of `rules`.
/// Returns whether anything changed.
fn remove_ask_first_rules(agent: &mut workspace_config::WorkspaceAgent, rules: &[String]) -> bool {
    let list = &mut agent.execution.shell.ask_first_command_prefixes;
    let before = list.len();
    list.retain(|entry| !rules.iter().any(|rule| rule.trim() == entry.trim()));
    list.len() != before
}

/// Emits the per-workspace pending-count update so the fleet card UI can
/// render a badge.
pub fn emit_attention(app: &tauri::AppHandle, workspace_id: Option<String>, pending_count: u32) {
    let payload = AttentionUpdate {
        workspace_id,
        pending_count,
    };
    if let Err(e) = app.emit(PERMISSION_ATTENTION_EVENT, payload) {
        tracing::warn!("Failed to emit permission attention event: {}", e);
    }
}

/// Tell the frontend to drop the approval card for `request_id`.
pub fn emit_permission_resolved(app: &tauri::AppHandle, request_id: &str) {
    if let Err(e) = app.emit(
        PERMISSION_RESOLVED_EVENT,
        serde_json::json!({ "requestId": request_id }),
    ) {
        tracing::warn!("Failed to emit permission resolved event: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_agent() -> workspace_config::WorkspaceAgent {
        workspace_config::WorkspaceAgent::new_manager("agent-1".to_string(), 1_000)
    }

    fn rules(rules: &[&str]) -> Vec<String> {
        rules.iter().map(|rule| rule.to_string()).collect()
    }

    #[test]
    fn always_allow_removes_every_matched_rule_and_only_those() {
        let mut agent = fake_agent();
        let before = agent.execution.shell.ask_first_command_prefixes.clone();
        assert!(before.contains(&"git push".to_string()));

        assert!(remove_ask_first_rules(
            &mut agent,
            &rules(&["git push", "rm"])
        ));

        let after = &agent.execution.shell.ask_first_command_prefixes;
        let expected: Vec<String> = before
            .iter()
            .filter(|rule| *rule != "git push" && *rule != "rm")
            .cloned()
            .collect();
        assert_eq!(after, &expected);
        // Removing them again changes nothing.
        assert!(!remove_ask_first_rules(&mut agent, &rules(&["git push"])));
    }

    #[test]
    fn always_allow_matches_a_saved_rule_with_surrounding_spaces() {
        let mut agent = fake_agent();
        agent.execution.shell.ask_first_command_prefixes = rules(&[" git push ", "rm"]);

        assert!(remove_ask_first_rules(&mut agent, &rules(&["git push"])));

        assert_eq!(
            agent.execution.shell.ask_first_command_prefixes,
            rules(&["rm"])
        );
    }

    #[test]
    fn denial_reason_is_trimmed_and_bounded() {
        let mut decision = AskFirstDecision::Deny {
            reason: Some("  use offline data  ".into()),
        };
        normalize_denial_reason(&mut decision).unwrap();
        assert_eq!(
            decision,
            AskFirstDecision::Deny {
                reason: Some("use offline data".into())
            }
        );

        let mut blank = AskFirstDecision::Deny {
            reason: Some("  ".into()),
        };
        normalize_denial_reason(&mut blank).unwrap();
        assert_eq!(blank, AskFirstDecision::Deny { reason: None });

        let mut at_limit = AskFirstDecision::Deny {
            reason: Some("é".repeat(500)),
        };
        assert!(normalize_denial_reason(&mut at_limit).is_ok());
        let mut too_long = AskFirstDecision::Deny {
            reason: Some("x".repeat(501)),
        };
        assert!(normalize_denial_reason(&mut too_long).is_err());
    }

    #[test]
    fn decisions_parse_from_the_wire() {
        let parse = |json: &str| serde_json::from_str::<AskFirstDecision>(json).unwrap();
        assert_eq!(
            parse(r#"{"kind":"allowOnce"}"#),
            AskFirstDecision::AllowOnce
        );
        assert_eq!(
            parse(r#"{"kind":"alwaysAllow"}"#),
            AskFirstDecision::AlwaysAllow
        );
        assert_eq!(
            parse(r#"{"kind":"deny"}"#),
            AskFirstDecision::Deny { reason: None }
        );
        assert_eq!(
            parse(r#"{"kind":"deny","reason":"not now"}"#),
            AskFirstDecision::Deny {
                reason: Some("not now".into())
            }
        );
    }

    #[test]
    fn request_rules_are_the_distinct_matched_rules() {
        let mut request = fake_request(Some("ws-1"));
        request.rows = vec![
            AskFirstRow {
                text: "git push origin a".into(),
                rule: "git push".into(),
            },
            AskFirstRow {
                text: "rm -f x".into(),
                rule: "rm".into(),
            },
            AskFirstRow {
                text: "git push origin b".into(),
                rule: "git push".into(),
            },
        ];
        assert_eq!(request.rules(), rules(&["git push", "rm"]));
    }

    fn fake_request(workspace_id: Option<&str>) -> PermissionRequest {
        PermissionRequest {
            persists_to_shared_agent: false,
            also_affects_workspaces: Vec::new(),
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.map(str::to_string),
            agent_id: None,
            agent_name: None,
            command: "cmd".to_string(),
            rows: vec![],
        }
    }

    #[tokio::test]
    async fn pending_approvals_register_and_take() {
        let pending = PendingApprovals::new();
        let req = fake_request(Some("ws-1"));
        let id = req.request_id.clone();
        let (_rx, count) = pending.register(req, "run-1".to_string()).await;
        assert_eq!(count, 1);
        let taken = pending.take(&id).await;
        assert!(taken.is_some());
        // Second take is a miss (entry removed).
        assert!(pending.take(&id).await.is_none());
    }

    #[tokio::test]
    async fn pending_approvals_count_increments_and_decrements_via_take() {
        let pending = PendingApprovals::new();
        let req1 = fake_request(Some("ws-1"));
        let req2 = fake_request(Some("ws-1"));
        let id1 = req1.request_id.clone();
        let id2 = req2.request_id.clone();
        let (_rx1, c1) = pending.register(req1, "run-1".to_string()).await;
        let (_rx2, c2) = pending.register(req2, "run-1".to_string()).await;
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
        let (_, remaining) = pending.take(&id1).await.unwrap();
        assert_eq!(remaining, 1);
        let (_, remaining) = pending.take(&id2).await.unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn pending_approvals_list_for_workspace_returns_matching_requests() {
        let pending = PendingApprovals::new();
        let req_a = fake_request(Some("ws-A"));
        let req_b = fake_request(Some("ws-B"));
        let _ = pending.register(req_a.clone(), "run-1".to_string()).await;
        let _ = pending.register(req_b, "run-1".to_string()).await;
        let list = pending.list_for_workspace("ws-A").await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, req_a.request_id);
    }

    #[tokio::test]
    async fn counts_snapshot_aggregates_by_workspace_and_drops_anon() {
        let pending = PendingApprovals::new();
        let _ = pending
            .register(fake_request(Some("ws-A")), "run-1".to_string())
            .await;
        let _ = pending
            .register(fake_request(Some("ws-A")), "run-1".to_string())
            .await;
        let _ = pending
            .register(fake_request(Some("ws-B")), "run-1".to_string())
            .await;
        let _ = pending
            .register(fake_request(None), "run-1".to_string())
            .await;

        let snapshot = pending.counts_snapshot().await;
        assert_eq!(snapshot.get("ws-A"), Some(&2));
        assert_eq!(snapshot.get("ws-B"), Some(&1));
        assert_eq!(snapshot.len(), 2);
    }

    #[tokio::test]
    async fn take_superseded_removes_only_same_run_and_command() {
        let pending = PendingApprovals::new();
        let stale = fake_request(Some("ws-1")); // command "cmd"
        let stale_id = stale.request_id.clone();
        let mut other_cmd = fake_request(Some("ws-1"));
        other_cmd.command = "different".to_string();
        let other_cmd_id = other_cmd.request_id.clone();
        let other_run = fake_request(Some("ws-1")); // command "cmd", run-2
        let other_run_id = other_run.request_id.clone();
        let (stale_rx, _) = pending.register(stale, "run-1".to_string()).await;
        let _rx2 = pending.register(other_cmd, "run-1".to_string()).await;
        let _rx3 = pending.register(other_run, "run-2".to_string()).await;

        let taken = pending.take_superseded("run-1", "cmd").await;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].request_id, stale_id);
        assert_eq!(
            taken[0].remaining, 2,
            "two unrelated entries must remain counted"
        );

        // Supersede is explicit; closed channels remain cancellation/teardown.
        assert!(matches!(
            stale_rx.await,
            Ok(PendingApprovalOutcome::Superseded)
        ));

        // Unrelated entries are untouched.
        assert!(pending.take(&other_cmd_id).await.is_some());
        assert!(pending.take(&other_run_id).await.is_some());
    }

    #[tokio::test]
    async fn take_superseded_returns_empty_when_nothing_matches() {
        let pending = PendingApprovals::new();
        let req = fake_request(Some("ws-1"));
        let id = req.request_id.clone();
        let _rx = pending.register(req, "run-1".to_string()).await;

        assert!(pending.take_superseded("run-9", "cmd").await.is_empty());
        assert!(pending.take_superseded("run-1", "other").await.is_empty());
        assert!(pending.take(&id).await.is_some(), "entry must survive");
    }

    #[tokio::test]
    async fn purge_workspace_cancels_run_before_dropping_sender() {
        let pending = PendingApprovals::new();
        let mut rx = pending
            .register(fake_request(Some("ws-1")), "run-1".to_string())
            .await
            .0;

        let run_ids = pending
            .purge_workspace_canceling_runs("ws-1", |run_id| {
                assert_eq!(run_id, "run-1");
                assert!(matches!(
                    rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
            })
            .await;

        assert_eq!(run_ids, vec!["run-1".to_string()]);
        assert!(
            rx.await.is_err(),
            "purge drops the sender after cancellation"
        );
    }
}

#[cfg(test)]
mod approval_routing_tests {
    //! A command allowance follows the teammate; a workspace's Main keeps its
    //! own.

    use super::*;
    use crate::commands::path_grants::approval_destination_tests::*;

    fn git_push() -> Vec<String> {
        vec!["git push".to_string()]
    }

    fn asks_first(shell: &crate::config::types::ShellCapabilityConfig) -> bool {
        shell
            .ask_first_command_prefixes
            .contains(&"git push".to_string())
    }

    #[test]
    fn a_teammates_always_allow_edits_the_shared_definition() {
        let fixture = Fixture::new();
        assert!(asks_first(&fixture.definition().behavior.execution.shell));

        persist_always_allow(&fixture.state, WORKSPACE_ID, ASSIGNMENT_ID, &git_push())
            .expect("persist");

        let definition = fixture.definition();
        assert!(!asks_first(&definition.behavior.execution.shell));
        assert_eq!(
            definition.revision, 2,
            "the shared edit is a new revision, so a stale form cannot revert it"
        );
        assert!(
            asks_first(&fixture.config().main_agent.expect("main").execution.shell),
            "the workspace Main keeps its own list"
        );
    }

    #[test]
    fn the_mains_always_allow_stays_in_its_workspace() {
        let fixture = Fixture::new();

        persist_always_allow(&fixture.state, WORKSPACE_ID, MAIN_ID, &git_push()).expect("persist");

        assert!(!asks_first(
            &fixture.config().main_agent.expect("main").execution.shell
        ));
        assert_eq!(
            fixture.definition().revision,
            1,
            "no shared definition was touched"
        );
        assert!(
            asks_first(
                &fixture
                    .other_config()
                    .main_agent
                    .expect("other main")
                    .execution
                    .shell
            ),
            "another workspace's Main is a different agent"
        );
    }

    #[test]
    fn the_prompt_states_where_an_always_decision_is_saved() {
        let fixture = Fixture::new();

        let teammate = PermissionRequest::for_agent(
            &fixture.state,
            Some(WORKSPACE_ID.to_string()),
            Some(ASSIGNMENT_ID.to_string()),
            "cargo test".to_string(),
            Vec::new(),
        );
        assert!(
            teammate.persists_to_shared_agent,
            "a teammate's allowance is saved on the shared definition"
        );
        assert_eq!(
            teammate.also_affects_workspaces.len(),
            1,
            "the second workspace has the same teammate: {:?}",
            teammate.also_affects_workspaces
        );

        let main = PermissionRequest::for_agent(
            &fixture.state,
            Some(WORKSPACE_ID.to_string()),
            Some(MAIN_ID.to_string()),
            "cargo test".to_string(),
            Vec::new(),
        );
        assert!(
            !main.persists_to_shared_agent,
            "a Main's allowance is its own, and the prompt must not claim otherwise"
        );
        assert!(main.also_affects_workspaces.is_empty());

        let unknown = PermissionRequest::for_agent(
            &fixture.state,
            Some(WORKSPACE_ID.to_string()),
            Some("nobody".to_string()),
            "cargo test".to_string(),
            Vec::new(),
        );
        assert!(!unknown.persists_to_shared_agent);
    }

    #[test]
    fn a_teammate_assigned_nowhere_else_still_saves_to_the_shared_agent() {
        let fixture = Fixture::new();
        // With the teammate removed from the second workspace the reach list is
        // empty — but the decision still lands on the definition, and every
        // workspace that assigns it later inherits it.
        fixture
            .state
            .update_workspace_config(OTHER_WORKSPACE_ID, |config| {
                config.assignments.clear();
                Ok(())
            })
            .expect("unassign elsewhere");

        let request = PermissionRequest::for_agent(
            &fixture.state,
            Some(WORKSPACE_ID.to_string()),
            Some(ASSIGNMENT_ID.to_string()),
            "cargo test".to_string(),
            Vec::new(),
        );

        assert!(request.persists_to_shared_agent);
        assert!(request.also_affects_workspaces.is_empty());
    }

    fn request_for(agent_id: &str) -> PermissionRequest {
        PermissionRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: Some(WORKSPACE_ID.to_string()),
            agent_id: Some(agent_id.to_string()),
            agent_name: None,
            command: "git push".to_string(),
            rows: vec![AskFirstRow {
                text: "git push".to_string(),
                rule: "git push".to_string(),
            }],
            persists_to_shared_agent: false,
            also_affects_workspaces: Vec::new(),
        }
    }

    async fn decide(fixture: &Fixture, agent_id: &str, decision: AskFirstDecision) {
        let request = request_for(agent_id);
        let pending = &fixture.state.pending_approvals;
        let (rx, _) = pending.register(request.clone(), "run-1".to_string()).await;
        resolve_pending_approval(
            &fixture.state,
            &request.request_id,
            decision.clone(),
            |_, _| {},
            |_, _| panic!("the request stays off the queue"),
        )
        .await
        .expect("deliver");
        let Ok(PendingApprovalOutcome::Decision(received)) = rx.await else {
            panic!("expected a decision");
        };
        assert_eq!(received, decision);
    }

    #[tokio::test]
    async fn always_allow_is_saved_before_the_command_is_released() {
        let fixture = Fixture::new();

        decide(&fixture, ASSIGNMENT_ID, AskFirstDecision::AlwaysAllow).await;

        assert!(!asks_first(&fixture.definition().behavior.execution.shell));
    }

    #[tokio::test]
    async fn allow_once_and_deny_save_nothing() {
        let fixture = Fixture::new();

        decide(&fixture, ASSIGNMENT_ID, AskFirstDecision::AllowOnce).await;
        decide(&fixture, MAIN_ID, AskFirstDecision::AllowOnce).await;
        decide(
            &fixture,
            ASSIGNMENT_ID,
            AskFirstDecision::Deny {
                reason: Some("no".into()),
            },
        )
        .await;
        decide(&fixture, MAIN_ID, AskFirstDecision::Deny { reason: None }).await;

        assert_eq!(fixture.definition().revision, 1);
        assert!(asks_first(&fixture.definition().behavior.execution.shell));
        assert!(asks_first(
            &fixture.config().main_agent.expect("main").execution.shell
        ));
    }

    #[tokio::test]
    async fn an_always_allow_that_cannot_be_saved_keeps_the_request_open() {
        let fixture = Fixture::new();
        // No agent by this id: the save fails.
        let request = request_for("nobody");
        let pending = &fixture.state.pending_approvals;
        let (mut rx, _) = pending.register(request.clone(), "run-1".to_string()).await;
        let mut left = Vec::new();
        let mut back = Vec::new();

        let error = resolve_pending_approval(
            &fixture.state,
            &request.request_id,
            AskFirstDecision::AlwaysAllow,
            |workspace, remaining| left.push((workspace, remaining)),
            |request, pending| back.push((request.request_id.clone(), pending)),
        )
        .await
        .expect_err("the save fails");

        assert!(error.contains("nobody"), "{error}");
        assert!(left.is_empty(), "the card stays open");
        assert_eq!(
            back,
            [(request.request_id.clone(), 1)],
            "a view that listed the queue during the save is told it is back"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "the command keeps waiting, and the run goes on"
        );
        assert_eq!(pending.list_for_workspace(WORKSPACE_ID).await.len(), 1);
        assert_eq!(pending.counts_snapshot().await.get(WORKSPACE_ID), Some(&1));

        // The user answers again from the same card.
        resolve_pending_approval(
            &fixture.state,
            &request.request_id,
            AskFirstDecision::AllowOnce,
            |workspace, remaining| left.push((workspace, remaining)),
            |_, _| panic!("the request stays off the queue"),
        )
        .await
        .expect("allow once");
        assert_eq!(left, [(Some(WORKSPACE_ID.to_string()), 0)]);
        assert!(matches!(
            rx.await,
            Ok(PendingApprovalOutcome::Decision(
                AskFirstDecision::AllowOnce
            ))
        ));
    }

    #[tokio::test]
    async fn a_failed_save_closes_the_request_when_nothing_waits_on_it() {
        let fixture = Fixture::new();
        let request = request_for("nobody");
        let pending = &fixture.state.pending_approvals;
        let (rx, _) = pending.register(request.clone(), "run-1".to_string()).await;
        drop(rx);
        let mut left = Vec::new();

        resolve_pending_approval(
            &fixture.state,
            &request.request_id,
            AskFirstDecision::AlwaysAllow,
            |workspace, remaining| left.push((workspace, remaining)),
            |_, _| panic!("nothing waits on the request"),
        )
        .await
        .expect_err("the save fails");

        assert_eq!(left, [(Some(WORKSPACE_ID.to_string()), 0)]);
        assert!(pending.list_for_workspace(WORKSPACE_ID).await.is_empty());
    }
}

#[cfg(test)]
mod denial_reason_tests {
    //! A denial reason reaches the model through the tool error, and is not
    //! written to agent configuration on the way.

    use super::*;
    use crate::commands::path_grants::approval_destination_tests::*;

    #[tokio::test]
    async fn a_submitted_reason_reaches_the_tool_error_and_is_not_persisted() {
        let fixture = Fixture::new();

        let rows = vec![AskFirstRow {
            text: "git push origin main".to_string(),
            rule: "git push".to_string(),
        }];
        let request = PermissionRequest {
            request_id: "req-1".to_string(),
            workspace_id: Some(WORKSPACE_ID.to_string()),
            agent_id: Some(MAIN_ID.to_string()),
            agent_name: None,
            command: "git push origin main".to_string(),
            rows: rows.clone(),
            persists_to_shared_agent: false,
            also_affects_workspaces: Vec::new(),
        };
        let before = fixture.config();
        // The bash tool registers the request and waits on the oneshot.
        let (rx, _count) = fixture
            .state
            .pending_approvals
            .register(request.clone(), "run-1".to_string())
            .await;

        // The user denies and gives a reason.
        let mut decision = AskFirstDecision::Deny {
            reason: Some(" no network upload ".to_string()),
        };
        normalize_denial_reason(&mut decision).expect("normalize reason");

        // The submit command removes the request from the queue and hands the
        // decision to the waiting tool.
        resolve_pending_approval(
            &fixture.state,
            &request.request_id,
            decision,
            |_, _| {},
            |_, _| panic!("the request stays off the queue"),
        )
        .await
        .expect("deliver");

        // The tool turns the decision into the error the model reads.
        let PendingApprovalOutcome::Decision(AskFirstDecision::Deny { reason }) =
            rx.await.expect("decision")
        else {
            panic!("expected a denial");
        };
        let error =
            crate::assistant::tools::local::ask_first_denied_message(&rows, reason.as_deref());
        assert_eq!(
            error,
            "User denied `git push origin main` (ask-first rule `git push`). \
             Reason: no network upload."
        );

        // The agent config is untouched, and the reason is not in it.
        let config = fixture.config();
        assert_eq!(
            serde_json::to_value(&config.main_agent).unwrap(),
            serde_json::to_value(&before.main_agent).unwrap()
        );
        let serialized = serde_json::to_string(&config).expect("serialize config");
        assert!(!serialized.contains("no network upload"), "{serialized}");
    }
}
