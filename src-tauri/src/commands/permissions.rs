//! Interactive shell-permission approval flow.
//!
//! When the bash tool runs a command that's not in the agent's allowlist
//! (Restricted mode), instead of silently denying, the backend:
//!
//! 1. Builds a [`PermissionRequest`] describing each pipeline segment that
//!    needs approval (with a suggested allowlist prefix per segment).
//! 2. Registers a [`oneshot::Sender`] in [`PendingApprovals`] keyed by a
//!    fresh request id.
//! 3. Emits [`PERMISSION_REQUEST_EVENT`] to the frontend and emits
//!    [`PERMISSION_ATTENTION_EVENT`] with the new per-workspace count so
//!    the fleet card can render a badge.
//! 4. `.await`s the oneshot (bounded by [`APPROVAL_TIMEOUT`]).
//! 5. When the frontend invokes [`submit_permission_decision`], the
//!    backend looks up the sender, writes any `AllowAlways`/`DenyAlways`
//!    entries to the workspace config for the request's agent *before*
//!    delivering the decisions, then sends the decisions through the
//!    oneshot. The bash tool resumes and either runs (all allows) or
//!    returns an error (any deny).
//!
//! Sequencing: persistence happens *before* the oneshot delivery so that a
//! crash between user click and command execution still leaves the grant
//! on disk. The next run sees the saved prefix and passes without
//! prompting.

#![allow(dead_code)] // wired into local::execute_bash_exec; some helpers also called from tests

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::config::workspace_config;
use crate::AppState;

pub const PERMISSION_REQUEST_EVENT: &str = "permissions://request";
pub const PERMISSION_ATTENTION_EVENT: &str = "permissions://attention";
/// Emitted when a pending request is cleared *without* a user decision —
/// the run was cancelled or ended (reaping a wait orphaned by a CLI
/// transport drop), the wait timed out, or a re-asked command superseded
/// the stale request. The inline approval card removes the now-useless
/// card on this. Normal user submissions remove the card optimistically
/// on the frontend, so they don't emit this.
pub const PERMISSION_RESOLVED_EVENT: &str = "permissions://resolved";

/// Maximum time the bash handler waits for a user response. Past this
/// point the awaiting tool treats the missing decision as abandoned and
/// stops the run. Direct-provider runs use this generous 24h hygiene
/// bound; CLI-backed runs apply a shorter timeout below the CLI MCP
/// client's own timeout so cleanup happens inside CLAI.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum PermissionScope {
    /// Persist for this workspace agent only, in
    /// `WorkspaceConfig.agents[].execution.shell`.
    Agent,
    // Workspace,  // deferred: requires workspace_id plumbing through
    //             // AgentConfig / SessionContext (runner.rs currently
    //             // populates workspace_id with the agent's own id).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum SegmentKind {
    /// A plain command segment whose prefix can be safely allowlisted.
    Simple,
    /// Substitutions / executor heads / redirects / control flow — must
    /// always go through fresh approval; can't be persisted.
    Opaque,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct SegmentApproval {
    pub text: String,
    pub kind: SegmentKind,
    pub suggested_prefix: String,
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
    pub segments: Vec<SegmentApproval>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct AttentionUpdate {
    pub workspace_id: Option<String>,
    pub pending_count: u32,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum SegmentDecision {
    AllowOnce,
    AllowAlways {
        scope: PermissionScope,
        prefix: String,
    },
    DenyOnce,
    DenyAlways {
        scope: PermissionScope,
        prefix: String,
    },
}

impl SegmentDecision {
    pub fn is_allow(&self) -> bool {
        matches!(
            self,
            SegmentDecision::AllowOnce | SegmentDecision::AllowAlways { .. }
        )
    }
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
    Decision(Vec<SegmentDecision>),
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
    /// `request_id` and the segment list), which is also stored so that
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

/// Tauri command — invoked by the frontend modal when the user submits
/// per-segment decisions. Persists any always-grant/always-deny entries
/// to disk *before* delivering the decisions to the awaiting bash tool,
/// so the grant survives a crash between click and command execution.
#[tauri::command]
pub async fn submit_permission_decision(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    request_id: String,
    decisions: Vec<SegmentDecision>,
) -> Result<(), String> {
    let Some((entry, remaining)) = state.pending_approvals.take(&request_id).await else {
        return Err(format!(
            "No pending approval with request_id `{}` (already resolved or timed out)",
            request_id
        ));
    };

    // Persist always-decisions before delivering so the grant survives a
    // crash between user click and command execution. If a request lacks
    // workspace/agent identity, there is intentionally no legacy fallback:
    // "always" decisions degrade to the current approval round only.
    if let (Some(workspace_id), Some(agent_id)) = (
        entry.workspace_id.as_deref(),
        entry.request.agent_id.as_deref(),
    ) {
        persist_decisions_to_agent(state.inner(), workspace_id, agent_id, &decisions)?;
    }
    let _ = entry
        .sender
        .send(PendingApprovalOutcome::Decision(decisions));
    emit_attention(&app, entry.workspace_id, remaining);
    Ok(())
}

/// Writes always-allow / always-deny decisions into the workspace config's
/// per-agent execution policy.
pub fn persist_decisions_to_agent(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    decisions: &[SegmentDecision],
) -> Result<(), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let mut config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id) else {
        return Err(format!("Workspace agent not found: {}", agent_id));
    };
    let mut changed = false;

    for decision in decisions {
        match decision {
            SegmentDecision::AllowAlways { prefix, .. } => {
                let prefix = prefix.trim();
                if prefix.is_empty() {
                    continue;
                }
                let before = agent.execution.shell.blocked_command_prefixes.len();
                agent
                    .execution
                    .shell
                    .blocked_command_prefixes
                    .retain(|p| p != prefix);
                changed |= agent.execution.shell.blocked_command_prefixes.len() != before;
                if !agent
                    .execution
                    .shell
                    .allowed_command_prefixes
                    .iter()
                    .any(|p| p == prefix)
                {
                    agent
                        .execution
                        .shell
                        .allowed_command_prefixes
                        .push(prefix.to_string());
                    changed = true;
                }
            }
            SegmentDecision::DenyAlways { prefix, .. } => {
                let prefix = prefix.trim();
                if prefix.is_empty() {
                    continue;
                }
                let before = agent.execution.shell.allowed_command_prefixes.len();
                agent
                    .execution
                    .shell
                    .allowed_command_prefixes
                    .retain(|p| p != prefix);
                changed |= agent.execution.shell.allowed_command_prefixes.len() != before;
                if !agent
                    .execution
                    .shell
                    .blocked_command_prefixes
                    .iter()
                    .any(|p| p == prefix)
                {
                    agent
                        .execution
                        .shell
                        .blocked_command_prefixes
                        .push(prefix.to_string());
                    changed = true;
                }
            }
            SegmentDecision::AllowOnce | SegmentDecision::DenyOnce => {}
        }
    }

    if changed {
        agent.updated_at = chrono::Utc::now().timestamp_millis();
        config.updated_at = agent.updated_at;
        workspace_config::save(&root, &config).map_err(|e| e.to_string())?;
        state
            .workspace_index
            .write()
            .map_err(|e| format!("Workspace index lock error: {}", e))?
            .insert_config(root, &config);
    }

    Ok(())
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

/// Tell the frontend to drop the inline approval card for `request_id`
/// because the request was cleared backend-side without a user decision.
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

    fn fake_request(workspace_id: Option<&str>) -> PermissionRequest {
        PermissionRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.map(str::to_string),
            agent_id: None,
            agent_name: None,
            command: "cmd".to_string(),
            segments: vec![],
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
