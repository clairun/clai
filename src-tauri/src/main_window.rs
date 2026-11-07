//! The app's single window: bringing it back to the front, and what closing
//! it does.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, Window, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::assistant::runtime::live_run_count;

/// Label of the window declared in `tauri.conf.json`, which sets none.
pub const MAIN_WINDOW: &str = "main";

/// Shows, unminimizes and focuses the main window.
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        tracing::warn!("Main window not found; cannot bring it to the front");
        return;
    };
    if let Err(e) = window
        .show()
        .and_then(|()| window.unminimize())
        .and_then(|()| window.set_focus())
    {
        tracing::warn!("Failed to bring the main window to the front: {}", e);
    }
}

/// What closing the main window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Hide the window and keep running.
    Hide,
    /// Ask first, because quitting interrupts the live runs.
    Confirm,
    /// Let the window close, which quits the app.
    Quit,
}

/// `keep_running` is whether CLAI should keep running when its window
/// closes; `can_hide` is whether something, such as the Dock or a tray icon,
/// can bring a hidden window back. Without one, hiding would leave a process
/// nobody can reach, so closing quits.
pub fn close_action(keep_running: bool, can_hide: bool, live_runs: usize) -> CloseAction {
    if keep_running && can_hide {
        CloseAction::Hide
    } else if live_runs > 0 {
        CloseAction::Confirm
    } else {
        CloseAction::Quit
    }
}

/// Set while the quit confirmation is on screen, so that another close
/// request does not open a second one.
static CONFIRM_OPEN: AtomicBool = AtomicBool::new(false);

/// Handles a close request on the main window: its close button, Alt+F4 or
/// the window manager. macOS Cmd+Q and Dock Quit do not send one, so they
/// quit without asking.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let WindowEvent::CloseRequested { api, .. } = event else {
        return;
    };
    if window.label() != MAIN_WINDOW {
        return;
    }
    let live_runs = live_run_count();
    // There is no setting yet, and nothing that could bring a hidden window
    // back.
    match close_action(false, false, live_runs) {
        CloseAction::Quit => {}
        CloseAction::Hide => {
            api.prevent_close();
            if let Err(e) = window.hide() {
                tracing::warn!("Failed to hide the main window: {}", e);
            }
        }
        CloseAction::Confirm => {
            api.prevent_close();
            confirm_quit(window, live_runs);
        }
    }
}

/// Asks whether to quit despite the live runs, and quits if so. Returns at
/// once: the dialog runs off the event loop and answers through a callback.
fn confirm_quit(window: &Window, live_runs: usize) {
    if CONFIRM_OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = window.app_handle().clone();
    window
        .dialog()
        .message(format!("{}. Quit anyway?", runs_in_progress(live_runs)))
        .title("Quit CLAI?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Quit anyway".to_string(),
            "Cancel".to_string(),
        ))
        .parent(window)
        .show(move |quit| {
            CONFIRM_OPEN.store(false, Ordering::Release);
            if quit {
                app.exit(0);
            }
        });
}

fn runs_in_progress(count: usize) -> String {
    if count == 1 {
        "1 run in progress".to_string()
    } else {
        format!("{count} runs in progress")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_action_hides_only_when_asked_to_and_able_to() {
        use CloseAction::{Confirm, Hide, Quit};
        // (keep_running, can_hide, live_runs, expected)
        let cases = [
            (true, true, 0, Hide),
            (true, true, 3, Hide),
            (true, false, 0, Quit),
            (true, false, 1, Confirm),
            (false, true, 0, Quit),
            (false, true, 2, Confirm),
            (false, false, 0, Quit),
            (false, false, 1, Confirm),
        ];
        for (keep_running, can_hide, live_runs, expected) in cases {
            assert_eq!(
                close_action(keep_running, can_hide, live_runs),
                expected,
                "keep_running={keep_running} can_hide={can_hide} live_runs={live_runs}"
            );
        }
    }

    #[test]
    fn runs_in_progress_agrees_in_number() {
        assert_eq!(runs_in_progress(1), "1 run in progress");
        assert_eq!(runs_in_progress(2), "2 runs in progress");
    }
}
