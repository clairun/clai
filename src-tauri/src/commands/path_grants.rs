//! Interactive filesystem-grant approval flow.
//!
//! Parallel to [`crate::commands::permissions`] but for *path access* rather
//! than *command prefixes*. When the agent calls `fs_request_grant`, the
//! backend:
//!
//! 1. Builds a [`PathGrantRequest`] (a single path + access level + the
//!    agent's reason for needing it).
//! 2. Registers a [`oneshot::Sender`] in [`PendingPathGrants`] keyed by a
//!    fresh request id, alongside server-resolved metadata (workspace id,
//!    agent id) so the frontend can't redirect persistence.
//! 3. Emits [`PATH_GRANT_REQUEST_EVENT`] to the frontend and
//!    [`PATH_GRANT_ATTENTION_EVENT`] with the new per-workspace count.
//! 4. `.await`s the oneshot (24h bound, matching the command flow).
//! 5. When the frontend invokes [`submit_path_grant_decision`], the
//!    decision is persisted *first* (if `AllowAlways`) by updating the
//!    agent's `execution.filesystem.extra_paths` in the DB, *then*
//!    delivered through the oneshot. The agent resumes; its
//!    `filesystem_grants()` will pick up the persisted grant on the next
//!    `fs_*` call, and the current run can also see it through the
//!    session-scoped grant container the tool maintains.
//!
//! Sequencing rationale: persist before deliver so a crash between user
//! click and tool resume still leaves the grant on disk. The next session
//! reads it from extra_paths. The lost in-flight tool call surfaces as a
//! failed tool result, which the agent can simply retry — and the retry
//! will see the grant already in place and short-circuit.

#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::commands::permissions::PermissionScope;
use crate::config::{workspace_config, FilesystemPathAccess, FilesystemPathGrant, GrantOrigin};
use crate::AppState;

pub const PATH_GRANT_REQUEST_EVENT: &str = "path-grants://request";
pub const PATH_GRANT_ATTENTION_EVENT: &str = "path-grants://attention";
/// Emitted when a pending path-grant request is cleared *without* a user
/// decision — the tool call was abandoned (CLI transport dropped mid-call,
/// run cancelled) or it timed out. The inline path-grant card removes the
/// now-useless card on this. Normal submissions clear the card optimistically
/// on the frontend, so they don't emit this.
pub const PATH_GRANT_RESOLVED_EVENT: &str = "path-grants://resolved";

/// Same bound as the command-approval flow: 24h is generous enough that
/// it never fires under normal interactive use and acts as a hygiene cap
/// for abandoned pending state.
pub const PATH_GRANT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct PathGrantRequest {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub requested_path: String,
    pub requested_access: FilesystemPathAccess,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct PathGrantAttentionUpdate {
    pub workspace_id: Option<String>,
    pub pending_count: u32,
}

/// User's response to a `PathGrantRequest`.
///
/// The frontend may narrow `path` (e.g. agent asked for `~/.config`, user
/// approves only `~/.config/gh`) and downgrade `access` (RW → RO). It must
/// never widen — the backend rejects approvals whose path is not equal to
/// or a descendant of the requested path, and whose access is not equal to
/// or weaker than the requested access. This keeps the trust model clear:
/// the modal can ratify or shrink what the agent asked for, never extend.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum PathGrantDecision {
    Deny,
    AllowOnce {
        /// May narrow (descendant of requested_path) but not widen.
        path: String,
        /// May downgrade (RW→RO) but not upgrade (RO→RW).
        access: FilesystemPathAccess,
    },
    AllowAlways {
        path: String,
        access: FilesystemPathAccess,
        scope: PermissionScope,
    },
}

impl PathGrantDecision {
    pub fn is_allow(&self) -> bool {
        matches!(
            self,
            PathGrantDecision::AllowOnce { .. } | PathGrantDecision::AllowAlways { .. }
        )
    }
}

pub struct PendingPathGrants {
    inner: AsyncMutex<PendingInner>,
}

struct PendingInner {
    entries: HashMap<String, PendingEntry>,
    counts: HashMap<Option<String>, u32>,
}

pub struct PendingEntry {
    pub sender: oneshot::Sender<PathGrantDecision>,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub request: PathGrantRequest,
}

impl PendingPathGrants {
    pub fn new() -> Self {
        Self {
            inner: AsyncMutex::new(PendingInner {
                entries: HashMap::new(),
                counts: HashMap::new(),
            }),
        }
    }

    pub async fn register(
        &self,
        request: PathGrantRequest,
    ) -> (oneshot::Receiver<PathGrantDecision>, u32) {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.inner.lock().await;
        let request_id = request.request_id.clone();
        let workspace_id = request.workspace_id.clone();
        let agent_id = request.agent_id.clone();
        inner.entries.insert(
            request_id,
            PendingEntry {
                sender: tx,
                workspace_id: workspace_id.clone(),
                agent_id,
                request,
            },
        );
        let entry = inner.counts.entry(workspace_id).or_insert(0);
        *entry += 1;
        let count = *entry;
        (rx, count)
    }

    pub async fn list_for_workspace(&self, workspace_id: &str) -> Vec<PathGrantRequest> {
        let inner = self.inner.lock().await;
        inner
            .entries
            .values()
            .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// See [`crate::commands::permissions::PendingApprovals::purge_workspace`].
    /// Same semantics — drops every pending path-grant request for the
    /// given workspace and clears its count. Used by `workspace_delete`.
    pub async fn purge_workspace(&self, workspace_id: &str) -> usize {
        let mut inner = self.inner.lock().await;
        let to_remove: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|(id, _)| id.clone())
            .collect();
        let count = to_remove.len();
        for id in to_remove {
            inner.entries.remove(&id);
        }
        inner.counts.remove(&Some(workspace_id.to_string()));
        count
    }

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

impl Default for PendingPathGrants {
    fn default() -> Self {
        Self::new()
    }
}

#[tauri::command]
pub async fn list_pending_path_grant_requests(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<PathGrantRequest>, String> {
    Ok(state
        .pending_path_grants
        .list_for_workspace(&workspace_id)
        .await)
}

/// Tauri command invoked by the frontend modal when the user resolves a
/// path-grant request. Validates the narrowing rules, persists `AllowAlways`
/// decisions to the agent's stored execution config in the DB, then
/// delivers the decision through the oneshot the awaiting tool is `.await`ing.
#[tauri::command]
pub async fn submit_path_grant_decision(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    request_id: String,
    decision: PathGrantDecision,
) -> Result<(), String> {
    let Some((entry, remaining)) = state.pending_path_grants.take(&request_id).await else {
        return Err(format!(
            "No pending path-grant with request_id `{}` (already resolved or timed out)",
            request_id
        ));
    };

    let validated = validate_decision_against_request(&entry.request, decision)?;

    // Persist *before* delivering. If the process dies between persist and
    // deliver, the next session reads the grant from extra_paths and the
    // agent's retry of fs_request_grant short-circuits because the path is
    // already granted.
    if let PathGrantDecision::AllowAlways {
        path,
        access,
        scope: _,
    } = &validated
    {
        let Some(agent_id) = entry.agent_id.as_deref() else {
            return Err("Cannot persist path grant: pending entry has no agent_id".to_string());
        };
        let Some(workspace_id) = entry.workspace_id.as_deref() else {
            return Err("Cannot persist path grant: pending entry has no workspace_id".to_string());
        };
        persist_grant_to_agent(
            state.inner(),
            workspace_id,
            agent_id,
            path,
            *access,
            &entry.request.reason,
        )?;
    }

    let _ = entry.sender.send(validated);
    emit_attention(&app, entry.workspace_id, remaining);
    Ok(())
}

fn validate_decision_against_request(
    request: &PathGrantRequest,
    decision: PathGrantDecision,
) -> Result<PathGrantDecision, String> {
    let (path, access) = match &decision {
        PathGrantDecision::Deny => return Ok(decision),
        PathGrantDecision::AllowOnce { path, access }
        | PathGrantDecision::AllowAlways { path, access, .. } => (path.as_str(), *access),
    };

    let requested = std::path::Path::new(&request.requested_path);
    let approved = std::path::Path::new(path);

    if approved != requested && !approved.starts_with(requested) {
        return Err(format!(
            "Approval path `{}` is not the requested path `{}` or a descendant of it; the modal may only narrow, not widen",
            path, request.requested_path
        ));
    }

    if matches!(request.requested_access, FilesystemPathAccess::ReadOnly)
        && matches!(access, FilesystemPathAccess::ReadWrite)
    {
        return Err(
            "Approval may not upgrade access from read_only to read_write; the modal may only downgrade"
                .to_string(),
        );
    }

    Ok(decision)
}

/// Reads the agent's execution config, appends a new `FilesystemPathGrant`
/// to `filesystem.extra_paths` (or upgrades an existing entry's access if
/// the path already exists at a weaker level), tags it with
/// `GrantOrigin::Approval`, and writes the JSON back. Idempotent: a second
/// approval for the same path+access pair is a no-op.
fn persist_grant_to_agent(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    path: &str,
    access: FilesystemPathAccess,
    reason: &str,
) -> Result<(), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {}", workspace_id))?;
    let mut config = workspace_config::load(&root).map_err(|e| e.to_string())?;
    let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id) else {
        return Err(format!(
            "Cannot persist path grant: workspace agent not found for id `{}`",
            agent_id
        ));
    };

    let execution = &mut agent.execution;

    if let Some(existing) = execution
        .filesystem
        .extra_paths
        .iter_mut()
        .find(|g| g.path == path)
    {
        let upgrades = matches!(existing.access, FilesystemPathAccess::ReadOnly)
            && matches!(access, FilesystemPathAccess::ReadWrite);
        if !upgrades && existing.access == access {
            return Ok(());
        }
        existing.access = access;
        existing.origin = Some(GrantOrigin::Approval {
            reason: reason.to_string(),
            granted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        });
    } else {
        execution.filesystem.extra_paths.push(FilesystemPathGrant {
            path: path.to_string(),
            access,
            origin: Some(GrantOrigin::Approval {
                reason: reason.to_string(),
                granted_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            }),
        });
    }

    agent.updated_at = chrono::Utc::now().timestamp_millis();
    config.updated_at = agent.updated_at;
    workspace_config::save(&root, &config).map_err(|e| e.to_string())?;
    state
        .workspace_index
        .write()
        .map_err(|e| format!("Workspace index lock error: {}", e))?
        .insert_config(root, &config);
    Ok(())
}

pub fn emit_attention(app: &tauri::AppHandle, workspace_id: Option<String>, pending_count: u32) {
    let payload = PathGrantAttentionUpdate {
        workspace_id,
        pending_count,
    };
    if let Err(e) = app.emit(PATH_GRANT_ATTENTION_EVENT, payload) {
        tracing::warn!("Failed to emit path-grant attention event: {}", e);
    }
}

/// Tell the frontend to drop the inline path-grant card for `request_id`
/// because the request was cleared backend-side without a user decision.
pub fn emit_path_grant_resolved(app: &tauri::AppHandle, request_id: &str) {
    if let Err(e) = app.emit(
        PATH_GRANT_RESOLVED_EVENT,
        serde_json::json!({ "requestId": request_id }),
    ) {
        tracing::warn!("Failed to emit path-grant resolved event: {}", e);
    }
}

/// Helper for tools that need to enrich the request id on outbound logs.
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Predicate exposed for `fs_request_grant` so the tool can short-circuit
/// when a path is already covered by existing grants (preset or extra_paths)
/// without bothering the user.
pub fn path_is_covered(
    grants: &[FilesystemPathGrant],
    path: &std::path::Path,
    required_access: FilesystemPathAccess,
) -> bool {
    grants.iter().any(|grant| {
        if !path_starts_with_or_equals(path, std::path::Path::new(&grant.path)) {
            return false;
        }
        access_satisfies(grant.access, required_access)
    })
}

fn path_starts_with_or_equals(candidate: &std::path::Path, root: &std::path::Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

fn access_satisfies(grant: FilesystemPathAccess, required: FilesystemPathAccess) -> bool {
    match (grant, required) {
        (FilesystemPathAccess::ReadWrite, _) => true,
        (FilesystemPathAccess::ReadOnly, FilesystemPathAccess::ReadOnly) => true,
        (FilesystemPathAccess::ReadOnly, FilesystemPathAccess::ReadWrite) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(path: &str, access: FilesystemPathAccess) -> PathGrantRequest {
        PathGrantRequest {
            request_id: "rq".to_string(),
            workspace_id: None,
            agent_id: None,
            agent_name: None,
            requested_path: path.to_string(),
            requested_access: access,
            reason: "test".to_string(),
        }
    }

    #[test]
    fn validation_allows_exact_match() {
        let request = req("/a/b", FilesystemPathAccess::ReadOnly);
        let decision = PathGrantDecision::AllowOnce {
            path: "/a/b".to_string(),
            access: FilesystemPathAccess::ReadOnly,
        };
        assert!(validate_decision_against_request(&request, decision).is_ok());
    }

    #[test]
    fn validation_allows_narrowing_to_descendant() {
        let request = req("/a", FilesystemPathAccess::ReadOnly);
        let decision = PathGrantDecision::AllowOnce {
            path: "/a/b".to_string(),
            access: FilesystemPathAccess::ReadOnly,
        };
        assert!(validate_decision_against_request(&request, decision).is_ok());
    }

    #[test]
    fn validation_rejects_widening_to_ancestor() {
        let request = req("/a/b", FilesystemPathAccess::ReadOnly);
        let decision = PathGrantDecision::AllowOnce {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadOnly,
        };
        assert!(validate_decision_against_request(&request, decision).is_err());
    }

    #[test]
    fn validation_rejects_unrelated_path() {
        let request = req("/a", FilesystemPathAccess::ReadOnly);
        let decision = PathGrantDecision::AllowOnce {
            path: "/b".to_string(),
            access: FilesystemPathAccess::ReadOnly,
        };
        assert!(validate_decision_against_request(&request, decision).is_err());
    }

    #[test]
    fn validation_allows_downgrade_rw_to_ro() {
        let request = req("/a", FilesystemPathAccess::ReadWrite);
        let decision = PathGrantDecision::AllowOnce {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadOnly,
        };
        assert!(validate_decision_against_request(&request, decision).is_ok());
    }

    #[test]
    fn validation_rejects_upgrade_ro_to_rw() {
        let request = req("/a", FilesystemPathAccess::ReadOnly);
        let decision = PathGrantDecision::AllowOnce {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadWrite,
        };
        assert!(validate_decision_against_request(&request, decision).is_err());
    }

    #[test]
    fn validation_passes_deny_through() {
        let request = req("/a", FilesystemPathAccess::ReadOnly);
        assert!(matches!(
            validate_decision_against_request(&request, PathGrantDecision::Deny).unwrap(),
            PathGrantDecision::Deny
        ));
    }

    #[test]
    fn path_is_covered_recognises_exact_match() {
        let grants = vec![FilesystemPathGrant {
            path: "/a/b".to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }];
        assert!(path_is_covered(
            &grants,
            std::path::Path::new("/a/b"),
            FilesystemPathAccess::ReadOnly,
        ));
    }

    #[test]
    fn path_is_covered_recognises_descendant_coverage() {
        let grants = vec![FilesystemPathGrant {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }];
        assert!(path_is_covered(
            &grants,
            std::path::Path::new("/a/b/c"),
            FilesystemPathAccess::ReadOnly,
        ));
    }

    #[test]
    fn path_is_covered_rejects_unrelated_path() {
        let grants = vec![FilesystemPathGrant {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }];
        assert!(!path_is_covered(
            &grants,
            std::path::Path::new("/b"),
            FilesystemPathAccess::ReadOnly,
        ));
    }

    #[test]
    fn path_is_covered_requires_rw_when_writing() {
        let grants = vec![FilesystemPathGrant {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadOnly,
            origin: None,
        }];
        assert!(!path_is_covered(
            &grants,
            std::path::Path::new("/a/b"),
            FilesystemPathAccess::ReadWrite,
        ));
    }

    #[test]
    fn path_is_covered_rw_grant_satisfies_ro_request() {
        let grants = vec![FilesystemPathGrant {
            path: "/a".to_string(),
            access: FilesystemPathAccess::ReadWrite,
            origin: None,
        }];
        assert!(path_is_covered(
            &grants,
            std::path::Path::new("/a/b"),
            FilesystemPathAccess::ReadOnly,
        ));
    }

    #[tokio::test]
    async fn pending_register_and_take_round_trip() {
        let pending = PendingPathGrants::new();
        let request = PathGrantRequest {
            request_id: "id-1".to_string(),
            workspace_id: Some("ws".to_string()),
            agent_id: Some("a".to_string()),
            agent_name: None,
            requested_path: "/p".to_string(),
            requested_access: FilesystemPathAccess::ReadOnly,
            reason: "r".to_string(),
        };
        let (_rx, count) = pending.register(request).await;
        assert_eq!(count, 1);
        let taken = pending.take("id-1").await;
        assert!(taken.is_some());
        let (_, remaining) = taken.unwrap();
        assert_eq!(remaining, 0);
        assert!(pending.take("id-1").await.is_none());
    }

    #[tokio::test]
    async fn pending_list_for_workspace_filters_by_id() {
        let pending = PendingPathGrants::new();
        let mut a = PathGrantRequest {
            request_id: "id-a".to_string(),
            workspace_id: Some("ws-A".to_string()),
            agent_id: None,
            agent_name: None,
            requested_path: "/p".to_string(),
            requested_access: FilesystemPathAccess::ReadOnly,
            reason: "r".to_string(),
        };
        let b = PathGrantRequest {
            request_id: "id-b".to_string(),
            workspace_id: Some("ws-B".to_string()),
            ..a.clone()
        };
        a.request_id = "id-a".to_string();
        let _ = pending.register(a).await;
        let _ = pending.register(b).await;
        let list = pending.list_for_workspace("ws-A").await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, "id-a");
    }
}
