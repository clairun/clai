//! OS notifications for requests that need a workspace's attention.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use tauri::{AppHandle, Emitter, Manager};
#[cfg(not(target_os = "linux"))]
use tauri_plugin_notification::NotificationExt;

use crate::window_core::WindowRegistry;
use crate::AppState;

#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Kind {
    Approval = 0,
    Path = 1,
    Network = 2,
    Question = 3,
}

#[derive(Default)]
struct Pending {
    counts: [u32; 4],
    sent: bool,
    active_id: Option<i32>,
}

#[derive(Default)]
struct HubData {
    pending: HashMap<String, Pending>,
    by_id: HashMap<i32, String>,
    next_id: i32,
}

#[derive(Default)]
pub struct AttentionNotifications {
    data: Mutex<HubData>,
    refresh_lock: AsyncMutex<()>,
    #[cfg(target_os = "linux")]
    linux: std::sync::Arc<crate::linux_notifications::LinuxNotifications>,
}

impl AttentionNotifications {
    pub fn schedule_refresh(app: &AppHandle, workspace_id: &str) {
        let app = app.clone();
        let workspace_id = workspace_id.to_owned();
        tauri::async_runtime::spawn(async move {
            if let Some(hub) = app.try_state::<Self>() {
                hub.refresh(&app, &workspace_id).await;
            }
        });
    }

    pub async fn refresh(&self, app: &AppHandle, workspace_id: &str) {
        self.refresh_counts(workspace_id, async {
            let state = app.state::<AppState>();
            let approvals = state.pending_approvals.counts_snapshot().await;
            let paths = state.pending_path_grants.counts_snapshot().await;
            let network = state.pending_network_requests.counts_snapshot().await;
            let questions = crate::assistant::tools::ask_user::pending_for_workspace(workspace_id);
            [
                approvals.get(workspace_id).copied().unwrap_or(0),
                paths.get(workspace_id).copied().unwrap_or(0),
                network.get(workspace_id).copied().unwrap_or(0),
                questions.len() as u32,
            ]
        })
        .await;
        self.reconcile(app);
    }

    async fn refresh_counts<F>(&self, workspace_id: &str, read: F)
    where
        F: Future<Output = [u32; 4]>,
    {
        let _refresh = self.refresh_lock.lock().await;
        let counts = read.await;
        self.data
            .lock()
            .expect("notification hub poisoned")
            .pending
            .entry(workspace_id.to_owned())
            .or_default()
            .counts = counts;
    }

    #[cfg(target_os = "linux")]
    pub fn delivery_unavailable(&self, id: i32) {
        let mut data = self.data.lock().expect("notification hub poisoned");
        data.delivery_unavailable(id);
    }

    pub async fn clear_deleted(&self, app: &AppHandle, workspace_id: &str) {
        let _refresh = self.refresh_lock.lock().await;
        {
            let mut data = self.data.lock().expect("notification hub poisoned");
            data.clear_workspace(workspace_id);
        }
        self.reconcile(app);
    }

    pub fn reconcile(&self, app: &AppHandle) {
        let focused = app.state::<WindowRegistry>().focused_workspace();
        let (show, close) = self
            .data
            .lock()
            .expect("notification hub poisoned")
            .plan(focused.as_deref());
        if !close.is_empty() {
            #[cfg(target_os = "linux")]
            self.linux.close(close);
            #[cfg(not(target_os = "linux"))]
            if let Err(error) = app.notification().remove_active(close) {
                tracing::warn!(%error, "Failed to close attention notification");
            }
        }
        for (id, workspace_id, has_question) in show {
            let title = app
                .state::<AppState>()
                .resolve_workspace_roster(&workspace_id)
                .map(|(workspace, _)| workspace.title)
                .unwrap_or_else(|_| "Workspace".to_owned());
            let body = if has_question {
                "has a question"
            } else {
                "needs your approval"
            };
            // Serialize dispatch with updates. Linux delivery separately tracks
            // cancellation until the server has assigned its notification ID.
            let data = self.data.lock().expect("notification hub poisoned");
            let still_pending = data
                .pending
                .get(&workspace_id)
                .is_some_and(|entry| entry.active_id == Some(id));
            if still_pending
                && app.state::<WindowRegistry>().focused_workspace().as_deref()
                    != Some(&workspace_id)
            {
                let body = format!("{title} {body}");
                #[cfg(target_os = "linux")]
                self.linux.show(app, id, body);
                #[cfg(not(target_os = "linux"))]
                if let Err(error) = app
                    .notification()
                    .builder()
                    .id(id)
                    .title("CLAI")
                    .body(body)
                    .show()
                {
                    tracing::warn!(%error, "Failed to show attention notification");
                }
            }
        }
    }

    fn workspace_for_click(&self, id: i32) -> Option<String> {
        self.data.lock().ok()?.by_id.get(&id).cloned()
    }
}

impl HubData {
    #[cfg(target_os = "linux")]
    fn delivery_unavailable(&mut self, id: i32) {
        let Some(workspace_id) = self.by_id.remove(&id) else {
            return;
        };
        if let Some(entry) = self.pending.get_mut(&workspace_id) {
            if entry.active_id == Some(id) {
                entry.active_id = None;
                entry.sent = false;
            }
        }
    }

    fn clear_workspace(&mut self, workspace_id: &str) {
        if let Some(entry) = self.pending.get_mut(workspace_id) {
            entry.counts = [0; 4];
        }
    }

    fn plan(&mut self, focused: Option<&str>) -> (Vec<(i32, String, bool)>, Vec<i32>) {
        let mut show = Vec::new();
        let mut close = Vec::new();
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        for workspace_id in ids {
            let entry = self
                .pending
                .get_mut(&workspace_id)
                .expect("pending workspace");
            let total: u32 = entry.counts.iter().sum();
            if total == 0 || focused == Some(workspace_id.as_str()) {
                if let Some(id) = entry.active_id.take() {
                    close.push(id);
                }
            }
            if total == 0 {
                self.by_id.retain(|_, workspace| workspace != &workspace_id);
                self.pending.remove(&workspace_id);
            } else if focused != Some(workspace_id.as_str()) && !entry.sent {
                self.next_id = self.next_id.wrapping_add(1).max(1);
                let id = self.next_id;
                let entry = self
                    .pending
                    .get_mut(&workspace_id)
                    .expect("pending workspace");
                entry.sent = true;
                entry.active_id = Some(id);
                self.by_id.insert(id, workspace_id.clone());
                show.push((id, workspace_id, entry.counts[Kind::Question as usize] > 0));
            }
        }
        (show, close)
    }
}

pub fn on_click(app: &AppHandle, id: i32) {
    let Some(workspace_id) = app
        .state::<AttentionNotifications>()
        .workspace_for_click(id)
    else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some((label, navigate)) = app.state::<WindowRegistry>().route_click(&workspace_id)
        else {
            return;
        };
        let Some(window) = app.get_webview_window(&label) else {
            return;
        };
        if navigate {
            if let Err(error) = app.emit_to(
                &label,
                "window://navigate",
                serde_json::json!({ "workspaceId": workspace_id }),
            ) {
                tracing::warn!(%error, "Failed to navigate from notification");
                return;
            }
        }
        if let Err(error) = window
            .show()
            .and_then(|()| window.unminimize())
            .and_then(|()| window.set_focus())
        {
            tracing::warn!(%error, "Failed to focus notification target");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notification_for_combined_pending_episode() {
        let mut hub = HubData::default();
        hub.pending.entry("ws".into()).or_default().counts[Kind::Approval as usize] = 1;
        let (shown, closed) = hub.plan(None);
        assert_eq!(shown.len(), 1);
        assert!(closed.is_empty());
        hub.pending.get_mut("ws").unwrap().counts[Kind::Question as usize] = 1;
        assert!(hub.plan(None).0.is_empty());
        hub.pending.get_mut("ws").unwrap().counts[Kind::Approval as usize] = 0;
        assert!(hub.plan(None).0.is_empty());
        hub.pending.get_mut("ws").unwrap().counts[Kind::Question as usize] = 0;
        assert_eq!(hub.plan(None).1, vec![shown[0].0]);
        assert!(!hub.pending.contains_key("ws"));
        hub.pending.entry("ws".into()).or_default().counts[Kind::Path as usize] = 1;
        assert_eq!(hub.plan(None).0.len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unavailable_service_retries_only_still_pending_episode() {
        let mut hub = HubData::default();
        hub.pending.entry("ws".into()).or_default().counts[0] = 1;
        let first = hub.plan(None).0[0].0;
        hub.delivery_unavailable(first);
        assert!(!hub.by_id.contains_key(&first));
        assert!(hub.plan(Some("ws")).0.is_empty());
        let second = hub.plan(None).0[0].0;
        assert_ne!(first, second);
        assert!(hub.plan(None).0.is_empty());
        hub.clear_workspace("ws");
        assert_eq!(hub.plan(None).1, vec![second]);
        hub.delivery_unavailable(second);
        assert!(hub.plan(None).0.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_delivery_cannot_reset_newer_or_resolved_episode() {
        let mut hub = HubData::default();
        hub.pending.entry("ws".into()).or_default().counts[0] = 1;
        let old = hub.plan(None).0[0].0;
        hub.delivery_unavailable(old);
        let current = hub.plan(None).0[0].0;
        hub.by_id.insert(old, "ws".into());
        hub.delivery_unavailable(old);
        assert_eq!(hub.pending["ws"].active_id, Some(current));
        assert!(hub.plan(None).0.is_empty());
        hub.clear_workspace("ws");
        hub.plan(None);
        hub.delivery_unavailable(current);
        assert!(!hub.pending.contains_key("ws"));
    }

    #[test]
    fn deletion_clears_episode_and_click_mapping() {
        let mut hub = HubData::default();
        hub.pending.entry("deleted".into()).or_default().counts[0] = 1;
        let (shown, _) = hub.plan(None);
        let id = shown[0].0;
        hub.clear_workspace("deleted");
        assert_eq!(hub.plan(None).1, vec![id]);
        assert!(!hub.pending.contains_key("deleted"));
        assert!(!hub.by_id.contains_key(&id));
    }

    #[tokio::test]
    async fn refreshes_cannot_apply_an_old_count_after_a_new_one() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::Arc;
        use tokio::sync::oneshot;

        let hub = Arc::new(AttentionNotifications::default());
        let current = Arc::new(AtomicU32::new(1));
        let (read_tx, read_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let first_hub = Arc::clone(&hub);
        let first_current = Arc::clone(&current);
        let first = tokio::spawn(async move {
            first_hub
                .refresh_counts("ws", async move {
                    let old = first_current.load(Ordering::SeqCst);
                    read_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    [old, 0, 0, 0]
                })
                .await;
        });
        read_rx.await.unwrap();
        current.store(0, Ordering::SeqCst);
        let second_read = Arc::new(AtomicBool::new(false));
        let second_hub = Arc::clone(&hub);
        let second_current = Arc::clone(&current);
        let second_mark = Arc::clone(&second_read);
        let second = tokio::spawn(async move {
            second_hub
                .refresh_counts("ws", async move {
                    second_mark.store(true, Ordering::SeqCst);
                    [second_current.load(Ordering::SeqCst), 0, 0, 0]
                })
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!second_read.load(Ordering::SeqCst));
        release_tx.send(()).unwrap();
        first.await.unwrap();
        second.await.unwrap();
        assert_eq!(hub.data.lock().unwrap().pending["ws"].counts[0], 0);
    }

    #[test]
    fn focus_suppresses_then_closes_without_reposting() {
        let mut hub = HubData::default();
        hub.pending.entry("ws".into()).or_default().counts[Kind::Network as usize] = 1;
        assert!(hub.plan(Some("ws")).0.is_empty());
        let (shown, _) = hub.plan(Some("other"));
        assert_eq!(shown.len(), 1);
        assert_eq!(hub.plan(Some("ws")).1, vec![shown[0].0]);
        // Native close is unavailable on macOS and Windows; a late click
        // still routes while the request remains pending.
        assert_eq!(hub.by_id.get(&shown[0].0).map(String::as_str), Some("ws"));
        assert!(hub.plan(None).0.is_empty());
        hub.pending.get_mut("ws").unwrap().counts[Kind::Network as usize] = 0;
        hub.plan(None);
        assert!(hub.by_id.is_empty());
    }
}
