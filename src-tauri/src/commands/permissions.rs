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
//!    backend looks up the sender, [`persist_decisions`] writes any
//!    `AllowAlways`/`DenyAlways` entries to `.clai/permissions.json`
//!    *before* delivering the decisions, then sends the decisions through
//!    the oneshot. The bash tool resumes and either runs (all allows) or
//!    returns an error (any deny).
//!
//! Sequencing: persistence happens *before* the oneshot delivery so that a
//! crash between user click and command execution still leaves the grant
//! on disk. The next run sees the saved prefix and passes without
//! prompting.

#![allow(dead_code)] // wired into local::execute_bash_exec; some helpers also called from tests

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::AppState;

pub const PERMISSION_REQUEST_EVENT: &str = "permissions://request";
pub const PERMISSION_ATTENTION_EVENT: &str = "permissions://attention";

/// Maximum time the bash handler waits for a user response. Past this
/// point we treat the request as fully denied (24h is generous enough
/// that it never fires during interactive use; it's a hygiene bound for
/// abandoned-pending state).
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionScope {
    /// Persist for this agent only. v1 stores in
    /// `<agent_workspace_root>/.clai/permissions.json`.
    Agent,
    // Workspace,  // deferred: requires workspace_id plumbing through
    //             // AgentConfig / SessionContext (runner.rs currently
    //             // populates workspace_id with the agent's own id).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SegmentKind {
    /// A plain command segment whose prefix can be safely allowlisted.
    Simple,
    /// Substitutions / executor heads / redirects / control flow — must
    /// always go through fresh approval; can't be persisted.
    Opaque,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentApproval {
    pub text: String,
    pub kind: SegmentKind,
    pub suggested_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub command: String,
    pub segments: Vec<SegmentApproval>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionUpdate {
    pub workspace_id: Option<String>,
    pub pending_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
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

/// In-memory registry of in-flight approval requests. Each entry holds
/// the workspace root path (resolved server-side at register time so the
/// frontend can't redirect persistence to an arbitrary directory) and a
/// oneshot sender used to deliver the user's decisions to the awaiting
/// bash tool.
pub struct PendingApprovals {
    inner: AsyncMutex<PendingInner>,
}

struct PendingInner {
    entries: HashMap<String, PendingEntry>,
    counts: HashMap<Option<String>, u32>,
}

pub struct PendingEntry {
    pub sender: oneshot::Sender<Vec<SegmentDecision>>,
    pub workspace_id: Option<String>,
    pub workspace_root: Option<PathBuf>,
    /// The original request payload as emitted to the frontend. Stored
    /// so that components that mount after the event fired can still
    /// discover the request via [`list_pending_permission_requests`].
    pub request: PermissionRequest,
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
        workspace_root: Option<PathBuf>,
    ) -> (oneshot::Receiver<Vec<SegmentDecision>>, u32) {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.inner.lock().await;
        let request_id = request.request_id.clone();
        let workspace_id = request.workspace_id.clone();
        inner.entries.insert(
            request_id,
            PendingEntry {
                sender: tx,
                workspace_id: workspace_id.clone(),
                workspace_root,
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

    /// Removes the pending entry and decrements its workspace count.
    /// Returns the entry (so callers can use the sender + workspace_root)
    /// and the post-decrement workspace count (for emitting attention).
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

    // Persist always-decisions before delivering — so the grant survives
    // a crash between user click and command execution. The workspace
    // root comes from server-side state, not the frontend; the frontend
    // can't redirect writes elsewhere.
    if let Some(root) = entry.workspace_root.as_deref() {
        persist_decisions(root, &decisions)?;
    }
    let _ = entry.sender.send(decisions);
    emit_attention(&app, entry.workspace_id, remaining);
    Ok(())
}

/// Writes always-allow / always-deny decisions to the per-agent
/// `.clai/permissions.json` file. Reconciliation: when adding to one
/// list, the same prefix is removed from the opposite list so the user's
/// state never accumulates contradictory entries at the same scope.
pub fn persist_decisions(
    workspace_root: &Path,
    decisions: &[SegmentDecision],
) -> Result<(), String> {
    use crate::assistant::tools::workspace_permissions::{load, save};

    let mut perms = load(workspace_root);
    let mut changed = false;

    for decision in decisions {
        match decision {
            SegmentDecision::AllowAlways { prefix, .. } => {
                let prefix = prefix.trim();
                if prefix.is_empty() {
                    continue;
                }
                // Reconcile: drop the same prefix from blocklist.
                let before = perms.shell.blocked_command_prefixes.len();
                perms.shell.blocked_command_prefixes.retain(|p| p != prefix);
                if perms.shell.blocked_command_prefixes.len() != before {
                    changed = true;
                }
                if !perms
                    .shell
                    .allowed_command_prefixes
                    .iter()
                    .any(|p| p == prefix)
                {
                    perms
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
                let before = perms.shell.allowed_command_prefixes.len();
                perms.shell.allowed_command_prefixes.retain(|p| p != prefix);
                if perms.shell.allowed_command_prefixes.len() != before {
                    changed = true;
                }
                if !perms
                    .shell
                    .blocked_command_prefixes
                    .iter()
                    .any(|p| p == prefix)
                {
                    perms
                        .shell
                        .blocked_command_prefixes
                        .push(prefix.to_string());
                    changed = true;
                }
            }
            SegmentDecision::AllowOnce | SegmentDecision::DenyOnce => {
                // Nothing to persist.
            }
        }
    }

    if changed {
        save(workspace_root, &perms).map_err(|e| {
            format!(
                "Failed to write workspace permissions at {}: {}",
                workspace_root.display(),
                e
            )
        })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::tools::workspace_permissions::{load, WorkspaceShellPermissions};
    use tempfile::TempDir;

    fn allow_always(prefix: &str) -> SegmentDecision {
        SegmentDecision::AllowAlways {
            scope: PermissionScope::Agent,
            prefix: prefix.to_string(),
        }
    }

    fn deny_always(prefix: &str) -> SegmentDecision {
        SegmentDecision::DenyAlways {
            scope: PermissionScope::Agent,
            prefix: prefix.to_string(),
        }
    }

    #[test]
    fn persist_writes_allow_to_disk() {
        let tmp = TempDir::new().unwrap();
        persist_decisions(tmp.path(), &[allow_always("git status")]).unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.shell.allowed_command_prefixes, vec!["git status"]);
    }

    #[test]
    fn persist_writes_deny_to_disk() {
        let tmp = TempDir::new().unwrap();
        persist_decisions(tmp.path(), &[deny_always("rm")]).unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.shell.blocked_command_prefixes, vec!["rm"]);
    }

    #[test]
    fn persist_reconciles_allow_against_existing_block() {
        let tmp = TempDir::new().unwrap();
        // Seed an existing block on `git status`.
        crate::assistant::tools::workspace_permissions::save(
            tmp.path(),
            &crate::assistant::tools::workspace_permissions::WorkspacePermissions {
                version: 1,
                shell: WorkspaceShellPermissions {
                    allowed_command_prefixes: vec![],
                    blocked_command_prefixes: vec!["git status".to_string()],
                },
            },
        )
        .unwrap();

        persist_decisions(tmp.path(), &[allow_always("git status")]).unwrap();

        let perms = load(tmp.path());
        assert_eq!(perms.shell.allowed_command_prefixes, vec!["git status"]);
        assert!(
            perms.shell.blocked_command_prefixes.is_empty(),
            "block of `git status` should be reconciled away on allow"
        );
    }

    #[test]
    fn persist_reconciles_deny_against_existing_allow() {
        let tmp = TempDir::new().unwrap();
        crate::assistant::tools::workspace_permissions::save(
            tmp.path(),
            &crate::assistant::tools::workspace_permissions::WorkspacePermissions {
                version: 1,
                shell: WorkspaceShellPermissions {
                    allowed_command_prefixes: vec!["kubectl logs".to_string()],
                    blocked_command_prefixes: vec![],
                },
            },
        )
        .unwrap();

        persist_decisions(tmp.path(), &[deny_always("kubectl logs")]).unwrap();

        let perms = load(tmp.path());
        assert!(perms.shell.allowed_command_prefixes.is_empty());
        assert_eq!(perms.shell.blocked_command_prefixes, vec!["kubectl logs"]);
    }

    #[test]
    fn persist_skips_once_decisions() {
        let tmp = TempDir::new().unwrap();
        persist_decisions(
            tmp.path(),
            &[SegmentDecision::AllowOnce, SegmentDecision::DenyOnce],
        )
        .unwrap();
        let perms = load(tmp.path());
        assert!(perms.shell.allowed_command_prefixes.is_empty());
        assert!(perms.shell.blocked_command_prefixes.is_empty());
    }

    #[test]
    fn persist_dedupes_within_same_call() {
        let tmp = TempDir::new().unwrap();
        persist_decisions(
            tmp.path(),
            &[allow_always("git status"), allow_always("git status")],
        )
        .unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.shell.allowed_command_prefixes, vec!["git status"]);
    }

    #[test]
    fn persist_dedupes_against_existing() {
        let tmp = TempDir::new().unwrap();
        crate::assistant::tools::workspace_permissions::save(
            tmp.path(),
            &crate::assistant::tools::workspace_permissions::WorkspacePermissions {
                version: 1,
                shell: WorkspaceShellPermissions {
                    allowed_command_prefixes: vec!["git status".to_string()],
                    blocked_command_prefixes: vec![],
                },
            },
        )
        .unwrap();

        persist_decisions(tmp.path(), &[allow_always("git status")]).unwrap();
        let perms = load(tmp.path());
        assert_eq!(perms.shell.allowed_command_prefixes, vec!["git status"]);
    }

    #[test]
    fn persist_ignores_empty_prefix() {
        let tmp = TempDir::new().unwrap();
        persist_decisions(tmp.path(), &[allow_always("   ")]).unwrap();
        let perms = load(tmp.path());
        assert!(perms.shell.allowed_command_prefixes.is_empty());
    }

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
        let (_rx, count) = pending.register(req, None).await;
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
        let (_rx1, c1) = pending.register(req1, None).await;
        let (_rx2, c2) = pending.register(req2, None).await;
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
        let (_, remaining) = pending.take(&id1).await.unwrap();
        assert_eq!(remaining, 1);
        let (_, remaining) = pending.take(&id2).await.unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn pending_approvals_remember_workspace_root() {
        let pending = PendingApprovals::new();
        let req = fake_request(Some("ws-1"));
        let id = req.request_id.clone();
        let (_rx, _) = pending
            .register(req, Some(PathBuf::from("/tmp/somewhere")))
            .await;
        let (entry, _) = pending.take(&id).await.unwrap();
        assert_eq!(entry.workspace_root, Some(PathBuf::from("/tmp/somewhere")));
    }

    #[tokio::test]
    async fn pending_approvals_list_for_workspace_returns_matching_requests() {
        let pending = PendingApprovals::new();
        let req_a = fake_request(Some("ws-A"));
        let req_b = fake_request(Some("ws-B"));
        let _ = pending.register(req_a.clone(), None).await;
        let _ = pending.register(req_b, None).await;
        let list = pending.list_for_workspace("ws-A").await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, req_a.request_id);
    }
}
