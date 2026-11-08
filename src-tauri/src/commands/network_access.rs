//! Network access requests.
//!
//! In Allowlist mode an agent that needs a host its lists do not cover calls
//! `net_request_access`. The tool builds a [`NetworkAccessRequest`],
//! registers it in [`PendingNetworkRequests`], emits
//! [`NETWORK_ACCESS_REQUEST_EVENT`] and waits. The card answers through
//! [`submit_network_access_decision`]:
//!
//! - *Allow this run*: the tool adds the host to the run's lists.
//! - *Always* (the host, or `*.` and its parent domain): the entry is added
//!   to the agent's allow list *before* the tool is released, so a crash in
//!   between still leaves it on disk. A teammate's list is its shared
//!   definition, the Main's is in its workspace, as for ask-first rules. A
//!   save that fails puts the request back on the queue and the card shows
//!   the error.
//! - *Deny*: the optional reason reaches the agent in the tool error and is
//!   not saved.
//!
//! A port on this computer (`localhost:<port>`) is asked as host
//! `localhost` with the port, and the card shows what listens there. The
//! request carries a [`LocalSnapshot`] of that listener beside the card's
//! view, and every answer binds to it, never to what listens when the user
//! answers: *Allow this run* adds the program, another user CLAI cannot see
//! into, or else the sockets, to the run; *Always* saves the program or the
//! other user as a local port grant, and is refused when only this run can
//! be allowed.
//!
//! The flow and the registry mirror the path-grant request
//! ([`crate::commands::path_grants`]); the registries are kept apart.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

#[cfg(target_os = "linux")]
use crate::assistant::network::local_listener::{Listener, RunOnly};
use crate::assistant::network::matcher::{
    approved_entry, is_local_name, parent_wildcard, parse_entry, parse_local_port, LOCALHOST,
};
use crate::assistant::network::run_state::HostLogEntry;
#[cfg(target_os = "linux")]
use crate::assistant::network::run_state::LocalBinding;
#[cfg(target_os = "linux")]
use crate::assistant::network::spawn_roots::StartedBy;
use crate::assistant::workspace_agents::workspace_agent_display_names;
use crate::commands::permissions::{normalize_reason, shared_allowance_reach, AttentionUpdate};
use crate::config::types::LocalPortGrant;
use crate::config::{workspace_config, NetworkPolicy};
use crate::AppState;

pub const NETWORK_ACCESS_REQUEST_EVENT: &str = "network-access://request";
pub const NETWORK_ACCESS_ATTENTION_EVENT: &str = "network-access://attention";
/// Emitted whenever a pending request leaves the queue: a decision, a
/// cancelled or ended run, or a re-asked request superseding it.
pub const NETWORK_ACCESS_RESOLVED_EVENT: &str = "network-access://resolved";

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct NetworkAccessRequest {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    /// As the matcher compares it: lower case, punycode, an IPv6 address
    /// without brackets.
    pub host: String,
    pub port: Option<u16>,
    pub reason: String,
    /// The hosts the run tried so far, for the card's “This run” line.
    pub run_hosts: Vec<HostLogEntry>,
    /// `*.` and the host's parent domain, when the card may offer it.
    pub suggested_wildcard: Option<String>,
    /// True when “Always” edits a shared agent definition rather than this
    /// workspace's Main (see `PermissionRequest`).
    pub persists_to_shared_agent: bool,
    /// The other workspaces that “Always” reaches today.
    pub also_affects_workspaces: Vec<String>,
    /// For a port on this computer (`host` is then `localhost`), what
    /// listens there: the card shows its view, and every answer binds to
    /// its snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<LocalListenerView>")]
    pub local_listener: Option<LocalListenerCard>,
}

impl NetworkAccessRequest {
    /// The request an agent waits on, with where “Always” would be saved.
    pub fn for_agent(
        state: &AppState,
        workspace_id: Option<String>,
        agent_id: Option<String>,
        host: String,
        port: Option<u16>,
        reason: String,
        run_hosts: Vec<HostLogEntry>,
    ) -> Self {
        let (agent_name, reach) = match (workspace_id.as_deref(), agent_id.as_deref()) {
            (Some(workspace), Some(agent)) => (
                workspace_agent_display_names(state, workspace).remove(agent),
                shared_allowance_reach(state, workspace, agent),
            ),
            _ => (None, None),
        };
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id,
            agent_id,
            agent_name,
            suggested_wildcard: parent_wildcard(&host),
            host,
            port,
            reason,
            run_hosts,
            persists_to_shared_agent: reach.is_some(),
            also_affects_workspaces: reach.unwrap_or_default(),
            local_listener: None,
        }
    }

    /// The request an agent waits on for `port` on this computer, with what
    /// listens there.
    #[cfg(target_os = "linux")]
    pub fn for_local_port(
        state: &AppState,
        workspace_id: Option<String>,
        agent_id: Option<String>,
        port: u16,
        reason: String,
        run_hosts: Vec<HostLogEntry>,
        listener: LocalListenerCard,
    ) -> Self {
        Self {
            local_listener: Some(listener),
            ..Self::for_agent(
                state,
                workspace_id,
                agent_id,
                LOCALHOST.to_string(),
                Some(port),
                reason,
                run_hosts,
            )
        }
    }
}

/// What listened on a port of this computer when the card was made: the
/// view the card shows and the snapshot every answer binds to, made
/// together so that neither reaches the registry without the other.
/// Serialised as the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalListenerCard {
    view: LocalListenerView,
    snapshot: LocalSnapshot,
}

impl Serialize for LocalListenerCard {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view.serialize(serializer)
    }
}

impl LocalListenerCard {
    /// The card for `listener`, or `None` when nothing or CLAI listens: no
    /// card is shown for those.
    #[cfg(target_os = "linux")]
    pub fn of(listener: &Listener) -> Option<Self> {
        let (snapshot, view) = match listener {
            Listener::Nothing | Listener::Clai => return None,
            Listener::Program(program) => (
                LocalSnapshot::Program {
                    exe: program.exe.clone(),
                },
                LocalListenerView::Program {
                    pid: program.pid,
                    exe: program.exe.clone(),
                    command_line: program.cmdline.clone(),
                    interpreter: program.interpreter,
                    started_by: program.started_by.clone().map(Into::into),
                },
            ),
            Listener::OtherUser { uid, name, sockets } => (
                LocalSnapshot::OtherUser {
                    uid: *uid,
                    name: name.clone(),
                    sockets: sockets.clone(),
                },
                LocalListenerView::OtherUser {
                    uid: *uid,
                    user_name: name.clone(),
                },
            ),
            Listener::RunOnly {
                reason,
                sockets,
                holders,
                started_by,
            } => (
                LocalSnapshot::RunOnly {
                    sockets: sockets.clone(),
                },
                LocalListenerView::RunOnly {
                    reason: (*reason).into(),
                    holders: holders
                        .iter()
                        .map(|holder| LocalHolderView {
                            pid: holder.pid,
                            exe: holder.exe.clone(),
                            command_line: holder.cmdline.clone(),
                        })
                        .collect(),
                    started_by: started_by.clone().map(Into::into),
                },
            ),
        };
        Some(Self { view, snapshot })
    }

    pub fn snapshot(&self) -> &LocalSnapshot {
        &self.snapshot
    }
}

/// What listened on a port of this computer when the card was made, as the
/// card shows it. Never sent to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "bindings.ts")]
#[cfg_attr(
    not(target_os = "linux"),
    expect(dead_code, reason = "only Linux finds what listens on a port")
)]
pub enum LocalListenerView {
    /// One program: “Always” saves its path.
    Program {
        pid: u32,
        exe: String,
        command_line: String,
        /// It runs whatever script, or relays wherever, it is told, so its
        /// path says little about the listener.
        interpreter: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        started_by: Option<LocalStartedBy>,
    },
    /// A service of another user, whose processes CLAI cannot see:
    /// “Always” saves the user.
    OtherUser { uid: u32, user_name: String },
    /// Nothing a saved grant can name: “Allow this run” only.
    RunOnly {
        reason: LocalRunOnlyReason,
        holders: Vec<LocalHolderView>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        started_by: Option<LocalStartedBy>,
    },
}

/// Who started a listener, when CLAI did: shown on the card, never part of
/// what an answer binds to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
#[cfg_attr(
    not(target_os = "linux"),
    expect(dead_code, reason = "only Linux finds what listens on a port")
)]
pub enum LocalStartedBy {
    /// A shell in the CLAI terminal.
    Terminal,
    /// An app opened from CLAI, by program name.
    App { name: String },
    /// An agent's shell command.
    Agent {
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        name: Option<String>,
    },
    /// An MCP server CLAI runs, by its configured name.
    McpServer { name: String },
}

/// Why a listener can be allowed for one run only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
#[cfg_attr(
    not(target_os = "linux"),
    expect(dead_code, reason = "only Linux finds what listens on a port")
)]
pub enum LocalRunOnlyReason {
    /// CLAI cannot see the processes behind the sockets (they are its own
    /// user's, or CLAI runs in a Flatpak or container).
    Unseen,
    /// A holder runs in a container or another sandbox.
    OtherNamespace,
    /// Different programs hold sockets on the port.
    SeveralPrograms,
    /// The program's file was replaced or deleted since it started.
    Replaced,
    /// The program path is not UTF-8.
    PathNotUtf8,
}

/// A process holding a socket of a run-only listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct LocalHolderView {
    pub pid: u32,
    pub exe: String,
    pub command_line: String,
}

/// What every answer to a request for a port on this computer binds to:
/// what listened when the card was made, never what listens when the user
/// answers. Kept in the request's [`LocalListenerCard`] and never
/// serialised; it holds no command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalSnapshot {
    Program {
        exe: String,
    },
    OtherUser {
        uid: u32,
        name: String,
        sockets: Vec<u64>,
    },
    RunOnly {
        sockets: Vec<u64>,
    },
}

impl LocalSnapshot {
    /// What “Allow this run” lets the run reach: the program, the other
    /// user as “Always” would save it, or else each socket CLAI could not
    /// tie to an owner it may name.
    #[cfg(target_os = "linux")]
    pub fn run_bindings(&self) -> Vec<LocalBinding> {
        match self {
            Self::Program { exe } => vec![LocalBinding::Program(exe.clone())],
            Self::OtherUser { uid, .. } if !is_clai_user(*uid) => vec![LocalBinding::User(*uid)],
            Self::OtherUser { sockets, .. } | Self::RunOnly { sockets } => sockets
                .iter()
                .map(|inode| LocalBinding::Socket(*inode))
                .collect(),
        }
    }

    /// The grant “Always” saves for `port`: the program, or the other user.
    /// Refused when only this run can be allowed, including for a listener
    /// of the user CLAI runs as, which a user grant must never name.
    pub fn always_grant(&self, port: u16) -> Result<LocalPortGrant, String> {
        match self {
            Self::Program { exe } => Ok(LocalPortGrant::Program {
                port,
                program: exe.clone(),
            }),
            Self::OtherUser { uid, name, .. } if !is_clai_user(*uid) => Ok(LocalPortGrant::User {
                port,
                uid: *uid,
                user: Some(name.clone()),
            }),
            _ => Err(format!(
                "Port {port} on this computer can be allowed for this run only: CLAI \
                 cannot name the program listening there."
            )),
        }
    }
}

#[cfg(target_os = "linux")]
impl From<StartedBy> for LocalStartedBy {
    fn from(by: StartedBy) -> Self {
        match by {
            StartedBy::Terminal => Self::Terminal,
            StartedBy::App(name) => Self::App { name },
            StartedBy::Agent(name) => Self::Agent { name },
            StartedBy::McpServer(name) => Self::McpServer { name },
        }
    }
}

#[cfg(target_os = "linux")]
impl From<RunOnly> for LocalRunOnlyReason {
    fn from(reason: RunOnly) -> Self {
        match reason {
            RunOnly::Unseen => Self::Unseen,
            RunOnly::OtherNamespace => Self::OtherNamespace,
            RunOnly::SeveralPrograms => Self::SeveralPrograms,
            RunOnly::Replaced => Self::Replaced,
            RunOnly::PathNotUtf8 => Self::PathNotUtf8,
        }
    }
}

#[cfg(unix)]
fn is_clai_user(uid: u32) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    uid == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn is_clai_user(_uid: u32) -> bool {
    true
}

/// The user's answer to a [`NetworkAccessRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub enum NetworkAccessDecision {
    /// Allow the host for the rest of this run.
    AllowRun,
    /// Add `entry` to the agent's allow list, and allow it in this run:
    /// the host itself, or `*.` and a parent domain of it that is not a
    /// public suffix (`approved_entry` in the network matcher).
    AllowAlways { entry: String },
    /// The reason goes back to the agent in the tool error.
    Deny {
        #[serde(default)]
        #[ts(optional)]
        reason: Option<String>,
    },
}

#[derive(Debug)]
pub enum PendingNetworkOutcome {
    Decision(NetworkAccessDecision),
    Superseded,
}

pub struct PendingNetworkEntry {
    pub sender: oneshot::Sender<PendingNetworkOutcome>,
    pub workspace_id: Option<String>,
    /// The run waiting on the decision, so a re-asked request supersedes it.
    pub run_id: String,
    pub request: NetworkAccessRequest,
}

pub struct SupersededNetworkRequest {
    pub request_id: String,
    pub workspace_id: Option<String>,
    pub remaining: u32,
}

/// In-flight network access requests, each with the sender its tool waits
/// on, and the pending count per workspace.
#[derive(Default)]
pub struct PendingNetworkRequests {
    inner: AsyncMutex<PendingInner>,
}

#[derive(Default)]
struct PendingInner {
    entries: HashMap<String, PendingNetworkEntry>,
    counts: HashMap<Option<String>, u32>,
}

impl PendingInner {
    /// Removes the entry and returns it with its workspace's new count.
    fn remove(&mut self, request_id: &str) -> Option<(PendingNetworkEntry, u32)> {
        let entry = self.entries.remove(request_id)?;
        let count = match self.counts.get_mut(&entry.workspace_id) {
            Some(count) if *count > 0 => {
                *count -= 1;
                let remaining = *count;
                if remaining == 0 {
                    self.counts.remove(&entry.workspace_id);
                }
                remaining
            }
            _ => 0,
        };
        Some((entry, count))
    }

    fn insert(&mut self, entry: PendingNetworkEntry) -> u32 {
        let count = self.counts.entry(entry.workspace_id.clone()).or_insert(0);
        *count += 1;
        let count = *count;
        self.entries.insert(entry.request.request_id.clone(), entry);
        count
    }
}

impl PendingNetworkRequests {
    /// Returns the receiver the tool waits on and the workspace's pending
    /// count.
    pub async fn register(
        &self,
        request: NetworkAccessRequest,
        run_id: String,
    ) -> (oneshot::Receiver<PendingNetworkOutcome>, u32) {
        let (sender, receiver) = oneshot::channel();
        let count = self.inner.lock().await.insert(PendingNetworkEntry {
            sender,
            workspace_id: request.workspace_id.clone(),
            run_id,
            request,
        });
        (receiver, count)
    }

    pub async fn list_for_workspace(&self, workspace_id: &str) -> Vec<NetworkAccessRequest> {
        let inner = self.inner.lock().await;
        inner
            .entries
            .values()
            .filter(|entry| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|entry| entry.request.clone())
            .collect()
    }

    /// The pending count per workspace id; requests without a workspace are
    /// left out.
    pub async fn counts_snapshot(&self) -> HashMap<String, u32> {
        let inner = self.inner.lock().await;
        inner
            .counts
            .iter()
            .filter_map(|(workspace, count)| workspace.as_ref().map(|id| (id.clone(), *count)))
            .collect()
    }

    /// Cancels the runs waiting on the workspace's requests, then drops the
    /// requests, and returns the cancelled run ids (see
    /// [`crate::commands::permissions::PendingApprovals::purge_workspace_canceling_runs`]).
    pub async fn purge_workspace_canceling_runs<F>(
        &self,
        workspace_id: &str,
        mut cancel_run: F,
    ) -> Vec<String>
    where
        F: FnMut(&str),
    {
        let mut inner = self.inner.lock().await;
        let ids: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| entry.workspace_id.as_deref() == Some(workspace_id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut run_ids: Vec<String> = ids
            .iter()
            .filter_map(|id| inner.entries.get(id).map(|entry| entry.run_id.clone()))
            .collect();
        run_ids.sort();
        run_ids.dedup();
        for run_id in &run_ids {
            cancel_run(run_id);
        }
        for id in ids {
            inner.entries.remove(&id);
        }
        inner.counts.remove(&Some(workspace_id.to_string()));
        run_ids
    }

    pub async fn request(&self, request_id: &str) -> Option<NetworkAccessRequest> {
        let inner = self.inner.lock().await;
        inner
            .entries
            .get(request_id)
            .map(|entry| entry.request.clone())
    }

    /// Removes the request; returns it with its workspace's new count.
    pub async fn take(&self, request_id: &str) -> Option<(PendingNetworkEntry, u32)> {
        self.inner.lock().await.remove(request_id)
    }

    /// Puts back a request [`take`](Self::take) removed, unless its tool
    /// stopped waiting meanwhile. Returns the workspace's pending count once
    /// it is pending again.
    pub async fn restore(&self, entry: PendingNetworkEntry) -> Option<u32> {
        let mut inner = self.inner.lock().await;
        if entry.sender.is_closed() {
            return None;
        }
        Some(inner.insert(entry))
    }

    /// Removes every request of the same run for the same host, and for a
    /// name of this computer the same port, waking each waiter with
    /// [`PendingNetworkOutcome::Superseded`]: the agent asked again after a
    /// CLI transport drop orphaned the first request.
    pub async fn take_superseded(
        &self,
        run_id: &str,
        host: &str,
        port: Option<u16>,
    ) -> Vec<SupersededNetworkRequest> {
        let per_port = is_local_name(host);
        let mut inner = self.inner.lock().await;
        let ids: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.run_id == run_id
                    && entry.request.host == host
                    && (!per_port || entry.request.port == port)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut taken = Vec::with_capacity(ids.len());
        for id in ids {
            let Some((entry, remaining)) = inner.remove(&id) else {
                continue;
            };
            let _ = entry.sender.send(PendingNetworkOutcome::Superseded);
            taken.push(SupersededNetworkRequest {
                request_id: id,
                workspace_id: entry.workspace_id,
                remaining,
            });
        }
        taken
    }
}

#[tauri::command]
pub async fn list_pending_network_access_requests(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<NetworkAccessRequest>, String> {
    Ok(state
        .pending_network_requests
        .list_for_workspace(&workspace_id)
        .await)
}

#[tauri::command]
pub async fn list_pending_network_access_counts(
    state: State<'_, AppState>,
) -> Result<HashMap<String, u32>, String> {
    Ok(state.pending_network_requests.counts_snapshot().await)
}

#[tauri::command]
pub async fn submit_network_access_decision(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    request_id: String,
    decision: NetworkAccessDecision,
) -> Result<(), String> {
    resolve_network_request(
        state.inner(),
        &request_id,
        decision,
        |workspace_id, remaining| {
            emit_network_access_resolved(&app, &request_id);
            emit_attention(&app, workspace_id, remaining);
        },
        |request, pending| {
            if let Err(e) = app.emit(NETWORK_ACCESS_REQUEST_EVENT, request) {
                tracing::warn!("Failed to emit network access request event: {}", e);
            }
            emit_attention(&app, request.workspace_id.clone(), pending);
        },
    )
    .await
}

/// Delivers `decision` to the tool waiting on `request_id`, saving an
/// “Always” first. A decision that does not fit the request is refused and
/// the request stays pending. `left_queue` gets the workspace and its
/// pending count once the request has left the queue; when the save fails,
/// the request goes back on the queue, `back_on_queue` gets it, and the
/// error is returned.
async fn resolve_network_request(
    state: &AppState,
    request_id: &str,
    decision: NetworkAccessDecision,
    left_queue: impl FnOnce(Option<String>, u32),
    back_on_queue: impl FnOnce(&NetworkAccessRequest, u32),
) -> Result<(), String> {
    let pending = &state.pending_network_requests;
    let Some(request) = pending.request(request_id).await else {
        return Err(no_pending_request(request_id));
    };
    let (decision, grant) = validate_decision(&request, decision)?;
    let Some((entry, remaining)) = pending.take(request_id).await else {
        return Err(no_pending_request(request_id));
    };
    if let NetworkAccessDecision::AllowAlways { entry: allowed } = &decision {
        let saved = match (
            entry.workspace_id.as_deref(),
            entry.request.agent_id.as_deref(),
        ) {
            (Some(workspace_id), Some(agent_id)) => match &grant {
                Some(grant) => persist_local_port(state, workspace_id, agent_id, grant),
                None => persist_network_allow(state, workspace_id, agent_id, allowed),
            },
            _ => Err("no agent to save it on; allow it for this run instead".to_string()),
        };
        if let Err(error) = saved {
            tracing::warn!("Could not save `{allowed}` on the agent: {error}");
            let request = entry.request.clone();
            match pending.restore(entry).await {
                Some(count) => back_on_queue(&request, count),
                None => left_queue(request.workspace_id, remaining),
            }
            return Err(format!("Could not save `{allowed}`: {error}"));
        }
    }
    left_queue(entry.workspace_id.clone(), remaining);
    let _ = entry.sender.send(PendingNetworkOutcome::Decision(decision));
    Ok(())
}

fn no_pending_request(request_id: &str) -> String {
    format!("No pending network access request `{request_id}` (already resolved or cleared)")
}

/// An “Always” entry must be one [`approved_entry`] accepts (stored in the
/// matcher's form). For a port on this computer it must name that port,
/// is delivered as `localhost:<port>`, and comes with the grant to save,
/// taken from the listener the card showed. A denial reason is trimmed and
/// bounded.
fn validate_decision(
    request: &NetworkAccessRequest,
    decision: NetworkAccessDecision,
) -> Result<(NetworkAccessDecision, Option<LocalPortGrant>), String> {
    let local = request
        .local_listener
        .as_ref()
        .map(LocalListenerCard::snapshot);
    match decision {
        NetworkAccessDecision::AllowRun => Ok((decision, None)),
        NetworkAccessDecision::AllowAlways { entry } => match local.zip(request.port) {
            Some((local, port)) => {
                if parse_local_port(&entry) != Ok(Some(port)) {
                    return Err(format!(
                        "`{entry}` is not port {port} on this computer, which is what \
                         “Always” saves."
                    ));
                }
                let grant = local.always_grant(port)?;
                let entry = format!("{LOCALHOST}:{port}");
                Ok((NetworkAccessDecision::AllowAlways { entry }, Some(grant)))
            }
            // Only a request made with the listener on the port can save
            // one; a host entry for this computer would be a stored no-op.
            None if is_local_name(&request.host) => Err(format!(
                "`{}` is this computer; “Always” saves one of its ports from a request \
                 that names it.",
                request.host
            )),
            None => {
                let entry = approved_entry(&request.host, &entry)?.normalized();
                Ok((NetworkAccessDecision::AllowAlways { entry }, None))
            }
        },
        NetworkAccessDecision::Deny { reason } => Ok((
            NetworkAccessDecision::Deny {
                reason: match reason {
                    Some(reason) => normalize_reason(&reason)?,
                    None => None,
                },
            },
            None,
        )),
    }
}

/// Adds `entry` to the agent's allow list where its settings live (see
/// [`persist_on_agent`]). Idempotent: an entry already on the list changes
/// nothing.
pub fn persist_network_allow(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    entry: &str,
) -> Result<(), String> {
    let entry = &parse_entry(entry)
        .map_err(|reason| format!("`{entry}` is not a valid host entry: {reason}"))?
        .normalized();
    persist_on_agent(state, workspace_id, agent_id, |policy| {
        add_allow_entry(policy, entry)
    })
}

/// Adds `grant` to the agent's local ports where its settings live (see
/// [`persist_on_agent`]). Idempotent: a grant already listed changes
/// nothing.
fn persist_local_port(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    grant: &LocalPortGrant,
) -> Result<(), String> {
    grant.check()?;
    persist_on_agent(state, workspace_id, agent_id, |policy| {
        add_local_port(policy, grant)
    })
}

/// Applies `add` to the agent's network policy where its settings live: a
/// teammate's shared definition, as its ask-first list (network access is
/// about the agent's job, not this project), or the workspace's Main.
/// When `add` reports a change, the write marks the agent changed (the
/// definition's revision, the Main's `updated_at`), so a settings form
/// opened before it cannot save over it.
fn persist_on_agent(
    state: &AppState,
    workspace_id: &str,
    agent_id: &str,
    add: impl Fn(&mut NetworkPolicy) -> bool,
) -> Result<(), String> {
    let root = state
        .workspace_root(workspace_id)
        .ok_or_else(|| format!("Workspace not found: {workspace_id}"))?;
    let workspace = workspace_config::load(&root).map_err(|e| e.to_string())?;
    if let Some(assignment) = workspace.assignment(agent_id) {
        return crate::commands::global_agents::update_definition(
            state,
            &assignment.agent_definition_id,
            |definition| {
                let behavior = &mut definition.behavior;
                if add(&mut behavior.execution.sandbox.network_access) {
                    definition.revision += 1;
                    behavior.updated_at = chrono::Utc::now().timestamp_millis();
                }
            },
        );
    }

    state.update_workspace_config(workspace_id, |config| {
        let Some(agent) = config
            .main_agent
            .as_mut()
            .filter(|agent| agent.id == agent_id)
        else {
            return Err(format!("Workspace agent not found: {agent_id}"));
        };
        if add(&mut agent.execution.sandbox.network_access) {
            let now = chrono::Utc::now().timestamp_millis();
            agent.touch(now);
            config.updated_at = now;
        }
        Ok(())
    })?;
    Ok(())
}

/// Appends `entry`, in the matcher's form, unless an entry for the same host
/// is already listed (in any form, such as Unicode from an older save).
/// Returns whether the list changed.
fn add_allow_entry(policy: &mut NetworkPolicy, entry: &str) -> bool {
    let listed = policy
        .allow_domains
        .iter()
        .any(|existing| parse_entry(existing).is_ok_and(|existing| existing.normalized() == entry));
    if !listed {
        policy.allow_domains.push(entry.to_string());
    }
    !listed
}

/// Appends `grant` unless the same grant is listed (a user grant under
/// another display name included). Returns whether the list changed.
fn add_local_port(policy: &mut NetworkPolicy, grant: &LocalPortGrant) -> bool {
    let listed = policy
        .local_ports
        .iter()
        .any(|existing| existing.same_grant(grant));
    if !listed {
        policy.local_ports.push(grant.clone());
    }
    !listed
}

pub fn emit_attention(app: &tauri::AppHandle, workspace_id: Option<String>, pending_count: u32) {
    let payload = AttentionUpdate {
        workspace_id,
        pending_count,
    };
    if let Err(e) = app.emit(NETWORK_ACCESS_ATTENTION_EVENT, payload) {
        tracing::warn!("Failed to emit network access attention event: {}", e);
    }
}

/// Tells every view showing the request to drop it.
pub fn emit_network_access_resolved(app: &tauri::AppHandle, request_id: &str) {
    if let Err(e) = app.emit(
        NETWORK_ACCESS_RESOLVED_EVENT,
        serde_json::json!({ "requestId": request_id }),
    ) {
        tracing::warn!("Failed to emit network access resolved event: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::path_grants::approval_destination_tests::*;

    fn request(
        workspace_id: Option<&str>,
        agent_id: Option<&str>,
        host: &str,
    ) -> NetworkAccessRequest {
        NetworkAccessRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace_id.map(str::to_string),
            agent_id: agent_id.map(str::to_string),
            agent_name: None,
            host: host.to_string(),
            port: Some(443),
            reason: "fixtures".to_string(),
            run_hosts: Vec::new(),
            suggested_wildcard: parent_wildcard(host),
            persists_to_shared_agent: false,
            also_affects_workspaces: Vec::new(),
            local_listener: None,
        }
    }

    fn allow_list(policy: &NetworkPolicy) -> &[String] {
        &policy.allow_domains
    }

    // --- the registry ----------------------------------------------------

    #[tokio::test]
    async fn register_take_and_counts() {
        let pending = PendingNetworkRequests::default();
        let first = request(Some("ws"), None, "a.example.org");
        let second = request(Some("ws"), None, "b.example.org");
        let other = request(Some("other"), None, "c.example.org");
        let anonymous = request(None, None, "d.example.org");
        assert_eq!(pending.register(first.clone(), "run".into()).await.1, 1);
        assert_eq!(pending.register(second, "run".into()).await.1, 2);
        let _ = pending.register(other, "run".into()).await;
        let _ = pending.register(anonymous, "run".into()).await;

        let counts = pending.counts_snapshot().await;
        assert_eq!(counts.len(), 2, "requests without a workspace are left out");
        assert_eq!(counts.get("ws"), Some(&2));
        assert_eq!(pending.list_for_workspace("ws").await.len(), 2);

        let (_, remaining) = pending.take(&first.request_id).await.expect("taken");
        assert_eq!(remaining, 1);
        assert!(pending.take(&first.request_id).await.is_none());
        assert_eq!(pending.counts_snapshot().await.get("ws"), Some(&1));
    }

    #[tokio::test]
    async fn take_superseded_matches_the_run_and_the_host_only() {
        let pending = PendingNetworkRequests::default();
        let stale = request(Some("ws"), None, "a.example.org");
        let other_host = request(Some("ws"), None, "b.example.org");
        let (stale_rx, _) = pending.register(stale.clone(), "run-1".into()).await;
        let _other_host = pending.register(other_host.clone(), "run-1".into()).await;
        let _other_run = pending
            .register(request(Some("ws"), None, "a.example.org"), "run-2".into())
            .await;

        // The port of a host does not matter.
        let taken = pending
            .take_superseded("run-1", "a.example.org", Some(80))
            .await;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].request_id, stale.request_id);
        assert_eq!(taken[0].remaining, 2, "the other requests stay counted");
        assert!(matches!(
            stale_rx.await,
            Ok(PendingNetworkOutcome::Superseded)
        ));
        assert!(pending.take(&other_host.request_id).await.is_some());
        assert_eq!(pending.list_for_workspace("ws").await.len(), 1);
    }

    #[tokio::test]
    async fn take_superseded_keeps_other_ports_of_this_computer() {
        let pending = PendingNetworkRequests::default();
        let on_port = |port| NetworkAccessRequest {
            port: Some(port),
            ..request(Some("ws"), None, "localhost")
        };
        let stale = on_port(5432);
        let other_port = on_port(6379);
        let _ = pending.register(stale.clone(), "run-1".into()).await;
        let _ = pending.register(other_port.clone(), "run-1".into()).await;

        let taken = pending
            .take_superseded("run-1", "localhost", Some(5432))
            .await;
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].request_id, stale.request_id);
        assert!(pending.take(&other_port.request_id).await.is_some());
    }

    #[tokio::test]
    async fn purge_cancels_the_run_before_dropping_the_request() {
        let pending = PendingNetworkRequests::default();
        let (mut rx, _) = pending
            .register(request(Some("ws-1"), None, "a.example.org"), "run-1".into())
            .await;
        let mut cancelled = Vec::new();

        let run_ids = pending
            .purge_workspace_canceling_runs("ws-1", |run_id| {
                assert!(matches!(
                    rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                cancelled.push(run_id.to_string());
            })
            .await;

        assert_eq!(cancelled, ["run-1"], "the run is cancelled");
        assert_eq!(run_ids, ["run-1".to_string()]);
        assert!(rx.await.is_err(), "the sender is dropped after the cancel");
        assert!(pending.counts_snapshot().await.is_empty());
    }

    // --- deciding --------------------------------------------------------

    async fn decide(
        fixture: &Fixture,
        request: &NetworkAccessRequest,
        decision: NetworkAccessDecision,
    ) -> Result<NetworkAccessDecision, String> {
        let pending = &fixture.state.pending_network_requests;
        let (rx, _) = pending.register(request.clone(), "run-1".into()).await;
        resolve_network_request(
            &fixture.state,
            &request.request_id,
            decision,
            |_, _| {},
            |_, _| panic!("the request stays off the queue"),
        )
        .await?;
        match rx.await {
            Ok(PendingNetworkOutcome::Decision(decision)) => Ok(decision),
            other => panic!("expected a decision, got {other:?}"),
        }
    }

    fn main_allow_list(fixture: &Fixture) -> Vec<String> {
        let main = fixture.config().main_agent.expect("main");
        allow_list(&main.execution.sandbox.network_access).to_vec()
    }

    fn definition_allow_list(fixture: &Fixture) -> Vec<String> {
        allow_list(
            &fixture
                .definition()
                .behavior
                .execution
                .sandbox
                .network_access,
        )
        .to_vec()
    }

    #[tokio::test]
    async fn a_teammates_always_edits_the_shared_definition() {
        let fixture = Fixture::new();
        let request = request(
            Some(WORKSPACE_ID),
            Some(ASSIGNMENT_ID),
            "mirror.example.org",
        );

        let delivered = decide(
            &fixture,
            &request,
            NetworkAccessDecision::AllowAlways {
                entry: "*.Example.org".to_string(),
            },
        )
        .await
        .expect("deliver");

        assert_eq!(
            delivered,
            NetworkAccessDecision::AllowAlways {
                entry: "*.example.org".to_string()
            },
            "the tool gets the entry in the matcher's form"
        );
        let definition = fixture.definition();
        assert!(definition_allow_list(&fixture).contains(&"*.example.org".to_string()));
        assert_eq!(
            definition.revision, 2,
            "a stale library form cannot revert it"
        );
        assert!(!main_allow_list(&fixture).contains(&"*.example.org".to_string()));
    }

    #[tokio::test]
    async fn the_mains_always_stays_in_its_workspace() {
        let fixture = Fixture::new();
        let request = request(Some(WORKSPACE_ID), Some(MAIN_ID), "mirror.example.org");

        decide(
            &fixture,
            &request,
            NetworkAccessDecision::AllowAlways {
                entry: "mirror.example.org".to_string(),
            },
        )
        .await
        .expect("deliver");

        assert!(main_allow_list(&fixture).contains(&"mirror.example.org".to_string()));
        assert_eq!(
            fixture.definition().revision,
            1,
            "no shared definition touched"
        );
        let other = fixture.other_config().main_agent.expect("other main");
        assert!(!allow_list(&other.execution.sandbox.network_access)
            .contains(&"mirror.example.org".to_string()));
    }

    #[test]
    fn saving_an_entry_already_listed_changes_nothing() {
        let fixture = Fixture::new();
        for _ in 0..2 {
            persist_network_allow(
                &fixture.state,
                WORKSPACE_ID,
                ASSIGNMENT_ID,
                "mirror.example.org",
            )
            .expect("persist");
        }
        persist_network_allow(
            &fixture.state,
            WORKSPACE_ID,
            ASSIGNMENT_ID,
            "MIRROR.example.org.",
        )
        .expect("persist");
        let listed = definition_allow_list(&fixture);
        assert_eq!(
            listed
                .iter()
                .filter(|entry| entry.as_str() == "mirror.example.org")
                .count(),
            1
        );
        assert_eq!(fixture.definition().revision, 2);
    }

    #[test]
    fn always_saves_punycode_and_finds_a_unicode_entry_already_listed() {
        let fixture = Fixture::new();
        persist_network_allow(&fixture.state, WORKSPACE_ID, MAIN_ID, "Bücher.example")
            .expect("persist");
        assert!(main_allow_list(&fixture).contains(&"xn--bcher-kva.example".to_string()));

        fixture
            .state
            .update_workspace_config(WORKSPACE_ID, |config| {
                let main = config.main_agent.as_mut().expect("main");
                main.execution.sandbox.network_access.allow_domains = vec!["über.example".into()];
                Ok(())
            })
            .expect("an older Unicode save");
        persist_network_allow(&fixture.state, WORKSPACE_ID, MAIN_ID, "xn--ber-goa.example")
            .expect("persist");
        assert_eq!(main_allow_list(&fixture), ["über.example"]);
    }

    #[tokio::test]
    async fn allow_run_and_deny_save_nothing() {
        let fixture = Fixture::new();
        let before_main = main_allow_list(&fixture);
        let before_definition = definition_allow_list(&fixture);
        for (agent, decision) in [
            (ASSIGNMENT_ID, NetworkAccessDecision::AllowRun),
            (MAIN_ID, NetworkAccessDecision::AllowRun),
            (MAIN_ID, NetworkAccessDecision::Deny { reason: None }),
        ] {
            let request = request(Some(WORKSPACE_ID), Some(agent), "mirror.example.org");
            decide(&fixture, &request, decision).await.expect("deliver");
        }
        assert_eq!(main_allow_list(&fixture), before_main);
        assert_eq!(definition_allow_list(&fixture), before_definition);
        assert_eq!(fixture.definition().revision, 1);
    }

    #[tokio::test]
    async fn a_wider_entry_is_refused_and_the_request_stays_pending() {
        let fixture = Fixture::new();
        let request = request(Some(WORKSPACE_ID), Some(MAIN_ID), "mirror.example.org");
        let pending = &fixture.state.pending_network_requests;
        let (mut rx, _) = pending.register(request.clone(), "run-1".into()).await;

        for entry in ["*.org", "example.org", "*.other.org"] {
            let error = resolve_network_request(
                &fixture.state,
                &request.request_id,
                NetworkAccessDecision::AllowAlways {
                    entry: entry.to_string(),
                },
                |_, _| panic!("the request stays on the queue"),
                |_, _| panic!("the request never left the queue"),
            )
            .await
            .expect_err("refused");
            assert!(error.contains(entry), "{error}");
        }

        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(pending.list_for_workspace(WORKSPACE_ID).await.len(), 1);
        assert!(!main_allow_list(&fixture).contains(&"*.org".to_string()));
    }

    /// The tool is released only once “Always” is on disk: when the save
    /// fails, the tool keeps waiting and the card can be answered again.
    #[tokio::test]
    async fn an_always_that_cannot_be_saved_does_not_release_the_tool() {
        let fixture = Fixture::new();
        // No agent by this id: the save fails.
        let request = request(Some(WORKSPACE_ID), Some("nobody"), "mirror.example.org");
        let pending = &fixture.state.pending_network_requests;
        let (mut rx, _) = pending.register(request.clone(), "run-1".into()).await;
        let mut left = Vec::new();
        let mut back = Vec::new();

        let error = resolve_network_request(
            &fixture.state,
            &request.request_id,
            NetworkAccessDecision::AllowAlways {
                entry: "mirror.example.org".to_string(),
            },
            |workspace, remaining| left.push((workspace, remaining)),
            |request, count| back.push((request.request_id.clone(), count)),
        )
        .await
        .expect_err("the save fails");

        assert!(error.contains("nobody"), "{error}");
        assert!(left.is_empty());
        assert_eq!(back, [(request.request_id.clone(), 1)]);
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        resolve_network_request(
            &fixture.state,
            &request.request_id,
            NetworkAccessDecision::AllowRun,
            |workspace, remaining| left.push((workspace, remaining)),
            |_, _| panic!("the request stays off the queue"),
        )
        .await
        .expect("allow this run");
        assert_eq!(left, [(Some(WORKSPACE_ID.to_string()), 0)]);
        assert!(matches!(
            rx.await,
            Ok(PendingNetworkOutcome::Decision(
                NetworkAccessDecision::AllowRun
            ))
        ));
    }

    /// Nothing would be saved, so “Always” is refused rather than reported
    /// as saved; the request stays answerable.
    #[tokio::test]
    async fn an_always_without_an_agent_is_refused_and_stays_pending() {
        let fixture = Fixture::new();
        let request = request(Some(WORKSPACE_ID), None, "mirror.example.org");
        let pending = &fixture.state.pending_network_requests;
        let (mut rx, _) = pending.register(request.clone(), "run-1".into()).await;
        let mut back = Vec::new();

        let error = resolve_network_request(
            &fixture.state,
            &request.request_id,
            NetworkAccessDecision::AllowAlways {
                entry: "mirror.example.org".to_string(),
            },
            |_, _| panic!("the request stays on the queue"),
            |request, count| back.push((request.request_id.clone(), count)),
        )
        .await
        .expect_err("nothing to save on");

        assert!(error.contains("no agent"), "{error}");
        assert_eq!(back, [(request.request_id.clone(), 1)]);
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    /// A save that fails after the tool stopped waiting leaves the queue
    /// instead of showing a card nobody waits on.
    #[tokio::test]
    async fn a_failed_save_whose_tool_is_gone_does_not_requeue() {
        let fixture = Fixture::new();
        let request = request(Some(WORKSPACE_ID), Some("nobody"), "mirror.example.org");
        let pending = &fixture.state.pending_network_requests;
        let (rx, _) = pending.register(request.clone(), "run-1".into()).await;
        drop(rx);
        let mut left = Vec::new();

        resolve_network_request(
            &fixture.state,
            &request.request_id,
            NetworkAccessDecision::AllowAlways {
                entry: "mirror.example.org".to_string(),
            },
            |workspace, remaining| left.push((workspace, remaining)),
            |_, _| panic!("no card for a tool that stopped waiting"),
        )
        .await
        .expect_err("the save fails");

        assert_eq!(left, [(Some(WORKSPACE_ID.to_string()), 0)]);
        assert!(pending.list_for_workspace(WORKSPACE_ID).await.is_empty());
    }

    #[tokio::test]
    async fn a_denial_reason_reaches_the_tool_error_and_is_not_saved() {
        let fixture = Fixture::new();
        let request = request(Some(WORKSPACE_ID), Some(MAIN_ID), "mirror.example.org");
        let before = serde_json::to_value(fixture.config().main_agent).unwrap();

        let delivered = decide(
            &fixture,
            &request,
            NetworkAccessDecision::Deny {
                reason: Some("  use the internal mirror ".to_string()),
            },
        )
        .await
        .expect("deliver");
        let NetworkAccessDecision::Deny { reason } = delivered else {
            panic!("expected a denial");
        };

        assert_eq!(
            crate::assistant::tools::local::network_denied_message(
                "mirror.example.org",
                reason.as_deref()
            ),
            "User denied network access to `mirror.example.org`. Reason: use the internal mirror."
        );
        let config = fixture.config();
        assert_eq!(serde_json::to_value(&config.main_agent).unwrap(), before);
        let saved = serde_json::to_string(&config).unwrap();
        assert!(!saved.contains("internal mirror"));
    }

    #[test]
    fn the_request_states_where_always_is_saved_and_what_it_may_widen_to() {
        let fixture = Fixture::new();
        let ask = |agent: &str, host: &str| {
            NetworkAccessRequest::for_agent(
                &fixture.state,
                Some(WORKSPACE_ID.to_string()),
                Some(agent.to_string()),
                host.to_string(),
                Some(443),
                "fixtures".to_string(),
                Vec::new(),
            )
        };
        let teammate = ask(ASSIGNMENT_ID, "mirror.example.org");
        assert_eq!(teammate.agent_name.as_deref(), Some("Reviewer"));
        assert!(teammate.persists_to_shared_agent);
        assert_eq!(teammate.also_affects_workspaces.len(), 1);
        assert_eq!(
            teammate.suggested_wildcard.as_deref(),
            Some("*.example.org")
        );

        let main = ask(MAIN_ID, "example.org");
        assert!(main.agent_name.is_some_and(|name| !name.is_empty()));
        assert!(!main.persists_to_shared_agent);
        assert!(main.also_affects_workspaces.is_empty());
        assert_eq!(main.suggested_wildcard, None);
    }

    // --- ports on this computer ------------------------------------------

    #[cfg(target_os = "linux")]
    mod local_ports {
        use super::*;
        use crate::assistant::network::local_listener::{Holder, Listener, Program, RunOnly};

        /// In every command line: the card shows it, nothing saves it.
        const SECRET: &str = "secret-host";

        fn own_uid() -> u32 {
            // SAFETY: geteuid has no preconditions and cannot fail.
            unsafe { libc::geteuid() }
        }

        fn ssh() -> Listener {
            Listener::Program(Program {
                exe: "/usr/bin/ssh".to_string(),
                pid: 4321,
                cmdline: format!("ssh -N -L 5432:db:5432 {SECRET}"),
                interpreter: false,
                started_by: None,
            })
        }

        fn postgres() -> Listener {
            Listener::OtherUser {
                uid: own_uid().wrapping_add(1),
                name: "postgres".to_string(),
                sockets: vec![11],
            }
        }

        fn run_only(reason: RunOnly) -> Listener {
            Listener::RunOnly {
                reason,
                sockets: vec![11],
                holders: vec![Holder {
                    pid: 77,
                    exe: "/usr/bin/rootlessport".to_string(),
                    cmdline: format!("rootlessport {SECRET}"),
                }],
                started_by: None,
            }
        }

        fn card(listener: &Listener) -> LocalListenerCard {
            LocalListenerCard::of(listener).expect("a card")
        }

        /// The request for port 5432, as `net_request_access` makes it.
        fn local_request(
            fixture: &Fixture,
            agent: &str,
            listener: &Listener,
        ) -> NetworkAccessRequest {
            NetworkAccessRequest::for_local_port(
                &fixture.state,
                Some(WORKSPACE_ID.to_string()),
                Some(agent.to_string()),
                5432,
                "tests".to_string(),
                Vec::new(),
                card(listener),
            )
        }

        fn always(entry: &str) -> NetworkAccessDecision {
            NetworkAccessDecision::AllowAlways {
                entry: entry.to_string(),
            }
        }

        async fn decide_local(
            fixture: &Fixture,
            request: &NetworkAccessRequest,
            decision: NetworkAccessDecision,
        ) -> Result<NetworkAccessDecision, String> {
            let pending = &fixture.state.pending_network_requests;
            let (rx, _) = pending.register(request.clone(), "run-1".into()).await;
            resolve_network_request(
                &fixture.state,
                &request.request_id,
                decision,
                |_, _| {},
                |_, _| panic!("the request stays off the queue"),
            )
            .await?;
            match rx.await {
                Ok(PendingNetworkOutcome::Decision(decision)) => Ok(decision),
                other => panic!("expected a decision, got {other:?}"),
            }
        }

        fn main_policy(fixture: &Fixture) -> NetworkPolicy {
            fixture
                .config()
                .main_agent
                .expect("main")
                .execution
                .sandbox
                .network_access
        }

        fn definition_policy(fixture: &Fixture) -> NetworkPolicy {
            fixture
                .definition()
                .behavior
                .execution
                .sandbox
                .network_access
        }

        #[test]
        fn the_card_shows_the_listener_and_the_snapshot_keeps_no_command_line() {
            assert_eq!(LocalListenerCard::of(&Listener::Nothing), None);
            assert_eq!(LocalListenerCard::of(&Listener::Clai), None);

            let shown = card(&ssh());
            assert_eq!(
                shown.snapshot(),
                &LocalSnapshot::Program {
                    exe: "/usr/bin/ssh".to_string()
                }
            );
            assert_eq!(
                serde_json::to_value(&shown).unwrap(),
                serde_json::json!({
                    "kind": "program",
                    "pid": 4321,
                    "exe": "/usr/bin/ssh",
                    "commandLine": format!("ssh -N -L 5432:db:5432 {SECRET}"),
                    "interpreter": false,
                })
            );
            let shown = card(&run_only(RunOnly::OtherNamespace));
            assert_eq!(
                shown.snapshot(),
                &LocalSnapshot::RunOnly { sockets: vec![11] }
            );
            assert_eq!(
                serde_json::to_value(&shown).unwrap(),
                serde_json::json!({
                    "kind": "runOnly",
                    "reason": "otherNamespace",
                    "holders": [{
                        "pid": 77,
                        "exe": "/usr/bin/rootlessport",
                        "commandLine": format!("rootlessport {SECRET}"),
                    }],
                })
            );
            // The request shows the view only: no snapshot, no sockets.
            let fixture = Fixture::new();
            let asked =
                serde_json::to_value(local_request(&fixture, MAIN_ID, &postgres())).unwrap();
            assert_eq!(asked["host"], "localhost");
            assert_eq!(asked["port"], 5432);
            assert_eq!(
                asked["localListener"],
                serde_json::json!({
                    "kind": "otherUser",
                    "uid": own_uid().wrapping_add(1),
                    "userName": "postgres",
                })
            );
            assert_eq!(asked["suggestedWildcard"], serde_json::Value::Null);
            let host = serde_json::to_value(request(None, None, "example.org")).unwrap();
            assert!(host.get("localListener").is_none(), "{host}");
        }

        #[test]
        fn the_card_says_who_started_the_listener_and_the_snapshot_ignores_it() {
            use crate::assistant::network::spawn_roots::StartedBy;
            let started = |listener: Listener, by: StartedBy| match listener {
                Listener::Program(program) => Listener::Program(Program {
                    started_by: Some(by),
                    ..program
                }),
                Listener::RunOnly {
                    reason,
                    sockets,
                    holders,
                    ..
                } => Listener::RunOnly {
                    reason,
                    sockets,
                    holders,
                    started_by: Some(by),
                },
                other => other,
            };
            for (by, shown) in [
                (
                    StartedBy::Terminal,
                    serde_json::json!({ "kind": "terminal" }),
                ),
                (
                    StartedBy::App("kitty".to_string()),
                    serde_json::json!({ "kind": "app", "name": "kitty" }),
                ),
                (
                    StartedBy::Agent(Some("Ana".to_string())),
                    serde_json::json!({ "kind": "agent", "name": "Ana" }),
                ),
                (
                    StartedBy::Agent(None),
                    serde_json::json!({ "kind": "agent" }),
                ),
                (
                    StartedBy::McpServer("browser".to_string()),
                    serde_json::json!({ "kind": "mcpServer", "name": "browser" }),
                ),
            ] {
                for listener in [ssh(), run_only(RunOnly::OtherNamespace)] {
                    let labelled = card(&started(listener.clone(), by.clone()));
                    assert_eq!(labelled.snapshot(), card(&listener).snapshot(), "{by:?}");
                    let view = serde_json::to_value(&labelled).unwrap();
                    assert_eq!(view["startedBy"], shown, "{view}");
                    let mut unlabelled = view.clone();
                    unlabelled.as_object_mut().unwrap().remove("startedBy");
                    assert_eq!(unlabelled, serde_json::to_value(card(&listener)).unwrap());
                }
            }
        }

        #[tokio::test]
        async fn a_teammates_always_saves_the_program_the_card_showed() {
            let fixture = Fixture::new();
            let before = definition_policy(&fixture).allow_domains;

            let delivered = decide_local(
                &fixture,
                &local_request(&fixture, ASSIGNMENT_ID, &ssh()),
                always("127.0.0.1:5432"),
            )
            .await
            .expect("deliver");

            assert_eq!(delivered, always("localhost:5432"));
            let saved = definition_policy(&fixture);
            assert_eq!(
                saved.local_ports,
                [LocalPortGrant::Program {
                    port: 5432,
                    program: "/usr/bin/ssh".to_string(),
                }]
            );
            assert_eq!(saved.allow_domains, before, "no host entry");
            assert_eq!(fixture.definition().revision, 2);
            let stored = serde_json::to_string(&fixture.definition()).unwrap();
            assert!(!stored.contains(SECRET), "{stored}");
            assert!(main_policy(&fixture).local_ports.is_empty());
        }

        #[tokio::test]
        async fn the_mains_always_saves_another_users_service_once() {
            let fixture = Fixture::new();
            for _ in 0..2 {
                decide_local(
                    &fixture,
                    &local_request(&fixture, MAIN_ID, &postgres()),
                    always("localhost:5432"),
                )
                .await
                .expect("deliver");
            }
            assert_eq!(
                main_policy(&fixture).local_ports,
                [LocalPortGrant::User {
                    port: 5432,
                    uid: own_uid().wrapping_add(1),
                    user: Some("postgres".to_string()),
                }]
            );
            assert_eq!(fixture.definition().revision, 1, "no shared definition");
        }

        /// Only “Allow this run” fits a listener no grant can name, a
        /// service of the user CLAI runs as included.
        #[tokio::test]
        async fn always_for_a_run_only_listener_is_refused_and_stays_pending() {
            let fixture = Fixture::new();
            let pending = &fixture.state.pending_network_requests;
            for listener in [
                run_only(RunOnly::Unseen),
                run_only(RunOnly::OtherNamespace),
                run_only(RunOnly::Replaced),
                Listener::OtherUser {
                    uid: own_uid(),
                    name: "me".to_string(),
                    sockets: vec![11],
                },
            ] {
                let request = local_request(&fixture, MAIN_ID, &listener);
                let (mut rx, _) = pending.register(request.clone(), "run-1".into()).await;
                let error = resolve_network_request(
                    &fixture.state,
                    &request.request_id,
                    always("localhost:5432"),
                    |_, _| panic!("the request stays on the queue"),
                    |_, _| panic!("the request never left the queue"),
                )
                .await
                .expect_err("refused");
                assert!(error.contains("this run only"), "{listener:?}: {error}");
                assert!(matches!(
                    rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
                assert!(pending.request(&request.request_id).await.is_some());

                resolve_network_request(
                    &fixture.state,
                    &request.request_id,
                    NetworkAccessDecision::AllowRun,
                    |_, _| {},
                    |_, _| panic!("the request stays off the queue"),
                )
                .await
                .expect("allow this run");
                assert!(matches!(
                    rx.await,
                    Ok(PendingNetworkOutcome::Decision(
                        NetworkAccessDecision::AllowRun
                    ))
                ));
            }
            assert!(main_policy(&fixture).local_ports.is_empty());
        }

        #[tokio::test]
        async fn always_for_a_local_port_saves_only_that_port() {
            let fixture = Fixture::new();
            let pending = &fixture.state.pending_network_requests;
            let asked = local_request(&fixture, MAIN_ID, &ssh());
            let _rx = pending.register(asked.clone(), "run-1".into()).await;
            for entry in [
                "localhost:6379",
                "[::1]:6379",
                "localhost",
                "*.localhost",
                "mirror.example.org",
            ] {
                let error = resolve_network_request(
                    &fixture.state,
                    &asked.request_id,
                    always(entry),
                    |_, _| panic!("the request stays on the queue"),
                    |_, _| panic!("the request never left the queue"),
                )
                .await
                .expect_err("refused");
                assert!(error.contains("port 5432"), "{entry}: {error}");
            }

            // A request for this computer without the listener on the port
            // saves no host entry for it.
            let bare = request(Some(WORKSPACE_ID), Some(MAIN_ID), "localhost");
            let _rx = pending.register(bare.clone(), "run-1".into()).await;
            let error = resolve_network_request(
                &fixture.state,
                &bare.request_id,
                always("localhost"),
                |_, _| panic!("the request stays on the queue"),
                |_, _| panic!("the request never left the queue"),
            )
            .await
            .expect_err("refused");
            assert!(error.contains("this computer"), "{error}");

            let policy = main_policy(&fixture);
            assert!(policy.local_ports.is_empty());
            assert!(!policy
                .allow_domains
                .iter()
                .any(|entry| entry.contains("localhost")));
        }
    }

    #[test]
    fn decisions_parse_from_the_wire() {
        for (wire, decision) in [
            (
                serde_json::json!({ "kind": "allowRun" }),
                NetworkAccessDecision::AllowRun,
            ),
            (
                serde_json::json!({ "kind": "allowAlways", "entry": "*.example.org" }),
                NetworkAccessDecision::AllowAlways {
                    entry: "*.example.org".to_string(),
                },
            ),
            (
                serde_json::json!({ "kind": "deny" }),
                NetworkAccessDecision::Deny { reason: None },
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<NetworkAccessDecision>(wire).unwrap(),
                decision
            );
        }
    }
}
