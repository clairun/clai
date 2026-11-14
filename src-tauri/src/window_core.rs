//! Equal-window state and window creation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::AppState;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", content = "workspaceId", rename_all = "camelCase")]
pub enum View {
    #[default]
    Other,
    Fleet,
    Workspace(String),
}

#[derive(Debug, Clone)]
struct WindowEntry {
    view: View,
    focus_seq: u64,
}

#[derive(Default)]
struct RegistryData {
    windows: HashMap<String, WindowEntry>,
    focused: Option<String>,
    next_focus_seq: u64,
    next_label: u64,
}

#[derive(Default)]
pub struct WindowRegistry(Mutex<RegistryData>);

impl WindowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, label: &str, view: View) {
        self.0
            .lock()
            .expect("window registry poisoned")
            .windows
            .insert(label.to_owned(), WindowEntry { view, focus_seq: 0 });
    }

    pub fn next_label(&self) -> String {
        let mut data = self.0.lock().expect("window registry poisoned");
        data.next_label += 1;
        format!("w-{}", data.next_label)
    }

    pub fn set_view(&self, label: &str, view: View) -> Result<(), String> {
        let mut data = self.0.lock().map_err(|e| e.to_string())?;
        let entry = data
            .windows
            .get_mut(label)
            .ok_or("Window is not registered")?;
        entry.view = view;
        Ok(())
    }

    pub fn focused(&self, label: &str, focused: bool) {
        let mut data = self.0.lock().expect("window registry poisoned");
        if focused {
            data.next_focus_seq += 1;
            let seq = data.next_focus_seq;
            if let Some(entry) = data.windows.get_mut(label) {
                entry.focus_seq = seq;
                data.focused = Some(label.to_owned());
            }
        } else if data.focused.as_deref() == Some(label) {
            data.focused = None;
        }
    }

    /// Returns whether another registered window remains after removal.
    pub fn remove(&self, label: &str) -> bool {
        let mut data = self.0.lock().expect("window registry poisoned");
        data.windows.remove(label);
        if data.focused.as_deref() == Some(label) {
            data.focused = None;
        }
        !data.windows.is_empty()
    }

    pub fn focused_workspace(&self) -> Option<String> {
        let data = self.0.lock().ok()?;
        let entry = data.windows.get(data.focused.as_deref()?)?;
        match &entry.view {
            View::Workspace(id) => Some(id.clone()),
            _ => None,
        }
    }

    pub fn route_click(&self, workspace_id: &str) -> Option<(String, bool)> {
        let data = self.0.lock().ok()?;
        let matching = data
            .windows
            .iter()
            .filter(|(_, entry)| matches!(&entry.view, View::Workspace(id) if id == workspace_id))
            .max_by_key(|(label, entry)| (entry.focus_seq, label.as_str()));
        if let Some((label, _)) = matching {
            return Some((label.clone(), false));
        }
        data.windows
            .iter()
            .max_by_key(|(label, entry)| (entry.focus_seq, label.as_str()))
            .map(|(label, _)| (label.clone(), true))
    }

    pub fn most_recent(&self) -> Option<String> {
        self.0
            .lock()
            .ok()?
            .windows
            .iter()
            .max_by_key(|(label, entry)| (entry.focus_seq, label.as_str()))
            .map(|(label, _)| label.clone())
    }

    pub fn showing(&self, workspace_id: &str) -> Vec<String> {
        self.0
            .lock()
            .expect("window registry poisoned")
            .windows
            .iter()
            .filter(|(_, entry)| entry.view == View::Workspace(workspace_id.to_owned()))
            .map(|(label, _)| label.clone())
            .collect()
    }
}

pub enum Launch {
    // The CLI phase will use this path for a new general window.
    #[allow(dead_code)]
    General,
    Workspace(String),
}

impl Launch {
    fn url(&self) -> PathBuf {
        match self {
            Self::General => PathBuf::from("/"),
            Self::Workspace(id) => PathBuf::from(format!("/?workspace={id}")),
        }
    }

    fn view(&self) -> View {
        match self {
            Self::General => View::Other,
            Self::Workspace(id) => View::Workspace(id.clone()),
        }
    }
}

pub fn open_window(app: &AppHandle, launch: Launch) -> Result<WebviewWindow, String> {
    let label = app.state::<WindowRegistry>().next_label();
    let mut config = app.config().app.windows[0].clone();
    config.label = label.clone();
    config.title = title(&launch.view(), app.state::<AppState>().inner());
    config.url = WebviewUrl::App(launch.url());
    let registry = app.state::<WindowRegistry>();
    registry.register(&label, launch.view());
    let window = match WebviewWindowBuilder::from_config(app, &config)
        .map_err(|e| e.to_string())
        .and_then(|builder| builder.build().map_err(|e| e.to_string()))
    {
        Ok(window) => window,
        Err(error) => {
            registry.remove(&label);
            return Err(error);
        }
    };
    if window.is_focused().unwrap_or(false) {
        registry.focused(&label, true);
    }
    Ok(window)
}

pub fn show_recent_window(app: &AppHandle) {
    let registry = app.state::<WindowRegistry>();
    let Some(label) = registry.most_recent() else {
        return;
    };
    if let Some(window) = app.get_webview_window(&label) {
        if let Err(error) = window
            .show()
            .and_then(|()| window.unminimize())
            .and_then(|()| window.set_focus())
        {
            tracing::warn!(%error, "Failed to focus window");
        }
    }
}

fn title(view: &View, state: &AppState) -> String {
    match view {
        View::Other => "CLAI".to_owned(),
        View::Fleet => "Fleet — CLAI".to_owned(),
        View::Workspace(id) => state
            .resolve_workspace_roster(id)
            .map(|(workspace, _)| format!("{} — CLAI", workspace.title))
            .unwrap_or_else(|_| "CLAI".to_owned()),
    }
}

#[tauri::command]
pub fn window_set_view(
    window: WebviewWindow,
    view: View,
    state: State<'_, AppState>,
    registry: State<'_, WindowRegistry>,
) -> Result<(), String> {
    let title = title(&view, state.inner());
    registry.set_view(window.label(), view)?;
    window.set_title(&title).map_err(|e| e.to_string())?;
    app_attention_reconcile(&window);
    Ok(())
}

#[tauri::command]
pub async fn window_open(
    app: AppHandle,
    workspace_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (workspace, _) = state.resolve_workspace_roster(&workspace_id)?;
    let window = open_window(&app, Launch::Workspace(workspace.id))?;
    Ok(window.label().to_owned())
}

fn app_attention_reconcile(window: &WebviewWindow) {
    window
        .app_handle()
        .state::<crate::attention_notifications::AttentionNotifications>()
        .reconcile(window.app_handle());
}

pub fn retitle_workspace(app: &AppHandle, id: &str, title: &str) {
    for label in app.state::<WindowRegistry>().showing(id) {
        if let Some(window) = app.get_webview_window(&label) {
            if let Err(error) = window.set_title(&format!("{title} — CLAI")) {
                tracing::warn!(%error, %label, "Failed to retitle window");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_click_uses_current_views_and_never_creates_a_window() {
        let registry = WindowRegistry::new();
        assert_eq!(registry.route_click("ws"), None);
        registry.register("a", View::Workspace("ws".into()));
        registry.register("b", View::Fleet);
        registry.focused("a", true);
        registry.focused("b", true);
        assert_eq!(registry.route_click("ws"), Some(("a".into(), false)));
        registry.set_view("a", View::Fleet).unwrap();
        assert_eq!(registry.route_click("ws"), Some(("b".into(), true)));
        registry
            .set_view("b", View::Workspace("ws".into()))
            .unwrap();
        assert_eq!(registry.route_click("ws"), Some(("b".into(), false)));
        registry
            .set_view("a", View::Workspace("ws".into()))
            .unwrap();
        assert_eq!(registry.route_click("ws"), Some(("b".into(), false)));
        registry.focused("a", true);
        assert_eq!(registry.focused_workspace().as_deref(), Some("ws"));
        assert_eq!(registry.route_click("ws"), Some(("a".into(), false)));
        registry.remove("b");
        registry.remove("a");
        assert_eq!(registry.route_click("ws"), None);
    }

    #[test]
    fn registry_tracks_equal_windows_and_focus_order() {
        let registry = WindowRegistry::new();
        registry.register("main", View::Fleet);
        registry.register("w-1", View::Workspace("a".into()));
        registry.focused("main", true);
        registry.focused("w-1", true);
        registry.focused("w-1", false);
        assert_eq!(registry.most_recent().as_deref(), Some("w-1"));
        registry
            .set_view("main", View::Workspace("a".into()))
            .unwrap();
        assert_eq!(registry.showing("a").len(), 2);
        registry.remove("w-1");
        assert_eq!(registry.most_recent().as_deref(), Some("main"));
        assert_eq!(registry.showing("a"), vec!["main"]);
    }

    #[test]
    fn removing_focused_window_reports_survivors_with_or_without_focus_loss() {
        for focus_lost_first in [false, true] {
            let registry = WindowRegistry::new();
            registry.register("closing", View::Workspace("pending".into()));
            registry.register("survivor", View::Fleet);
            registry.focused("closing", true);
            if focus_lost_first {
                registry.focused("closing", false);
            }
            assert!(registry.remove("closing"));
            assert_eq!(registry.focused_workspace(), None);
            assert_eq!(registry.most_recent().as_deref(), Some("survivor"));
            assert!(!registry.remove("survivor"));
        }
    }

    #[test]
    fn labels_are_unique_after_window_removal() {
        let registry = WindowRegistry::new();
        assert_eq!(registry.next_label(), "w-1");
        registry.register("w-1", View::Other);
        registry.remove("w-1");
        assert_eq!(registry.next_label(), "w-2");
    }

    #[test]
    fn launch_url_uses_root_query() {
        assert_eq!(
            Launch::Workspace("abc".into()).url(),
            PathBuf::from("/?workspace=abc")
        );
        assert_eq!(Launch::General.url(), PathBuf::from("/"));
    }
}
