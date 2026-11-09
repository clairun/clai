//! Window close handling, including the last-window Quit prompt.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::{Manager, Window, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::assistant::runtime::live_run_count;

/// What closing the last window does.
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

/// What the last window does instead of closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeepOpen {
    Hide,
    /// Ask whether to quit.
    Ask,
    /// Nothing more: the question is already on screen.
    Wait,
}

/// The quit question: at most one on screen, and closing again once it is
/// answered asks again.
struct QuitPrompt {
    open: AtomicBool,
    closing: Mutex<Vec<String>>,
}

impl QuitPrompt {
    const fn new() -> Self {
        Self {
            open: AtomicBool::new(false),
            closing: Mutex::new(Vec::new()),
        }
    }

    /// `None` lets the window close. Accepted closes stay pending until Destroyed,
    /// since Tauri still counts those windows during later close requests.
    fn on_close_requested(
        &self,
        label: &str,
        window_count: usize,
        action: CloseAction,
    ) -> Option<KeepOpen> {
        let mut closing = self.closing.lock().expect("quit prompt poisoned");
        if self.open.load(Ordering::Acquire) {
            return Some(KeepOpen::Wait);
        }
        if closing.iter().any(|pending| pending == label) {
            return None;
        }
        if window_count.saturating_sub(closing.len()) > 1 {
            closing.push(label.to_owned());
            return None;
        }
        let keep_open = match action {
            CloseAction::Quit => None,
            CloseAction::Hide => Some(KeepOpen::Hide),
            CloseAction::Confirm => {
                if self.open.swap(true, Ordering::AcqRel) {
                    Some(KeepOpen::Wait)
                } else {
                    Some(KeepOpen::Ask)
                }
            }
        };
        if keep_open.is_none() {
            closing.push(label.to_owned());
        }
        keep_open
    }

    fn on_destroyed(&self, label: &str) {
        self.closing
            .lock()
            .expect("quit prompt poisoned")
            .retain(|pending| pending != label);
    }

    /// Takes the answer, calling `exit` if the user chose to quit.
    fn on_answer(&self, quit: bool, exit: impl FnOnce()) {
        self.open.store(false, Ordering::Release);
        if quit {
            exit();
        }
    }
}

static QUIT_PROMPT: QuitPrompt = QuitPrompt::new();

/// Handles focus, destruction and a close request on the last window: its close button, Alt+F4 or
/// the window manager. macOS Cmd+Q and Dock Quit do not send one, so they
/// quit without asking.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    let registry = window
        .app_handle()
        .state::<crate::window_core::WindowRegistry>();
    match event {
        WindowEvent::Focused(focused) => {
            registry.focused(window.label(), *focused);
            return;
        }
        WindowEvent::Destroyed => {
            QUIT_PROMPT.on_destroyed(window.label());
            registry.remove(window.label());
            window
                .app_handle()
                .state::<crate::AppState>()
                .terminals
                .close_window(window.label());
            return;
        }
        WindowEvent::CloseRequested { .. } => {}
        _ => return,
    }
    let WindowEvent::CloseRequested { api, .. } = event else {
        unreachable!()
    };
    let live_runs = live_run_count();
    // There is no setting yet, and nothing that could bring a hidden window
    // back.
    let Some(keep_open) = QUIT_PROMPT.on_close_requested(
        window.label(),
        window.app_handle().webview_windows().len(),
        close_action(false, false, live_runs),
    ) else {
        return;
    };
    api.prevent_close();
    match keep_open {
        KeepOpen::Hide => {
            if let Err(e) = window.hide() {
                tracing::warn!("Failed to hide the last window: {}", e);
            }
        }
        KeepOpen::Ask => ask_to_quit(window, live_runs),
        KeepOpen::Wait => {}
    }
}

/// Asks whether to quit despite the live runs. The dialog answers asynchronously.
fn ask_to_quit(window: &Window, live_runs: usize) {
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
        .show(move |quit| QUIT_PROMPT.on_answer(quit, || app.exit(0)));
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
    use std::cell::Cell;

    use super::*;

    #[test]
    fn simultaneous_closes_guard_the_effective_last_window() {
        let prompt = QuitPrompt::new();
        assert_eq!(
            prompt.on_close_requested("a", 2, CloseAction::Confirm),
            None
        );
        // Tauri has not delivered Destroyed for a, so both windows are still counted.
        assert_eq!(
            prompt.on_close_requested("b", 2, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
        assert_eq!(
            prompt.on_close_requested("b", 2, CloseAction::Confirm),
            Some(KeepOpen::Wait)
        );
        prompt.on_destroyed("a");
        prompt.on_answer(false, || panic!("cancel must not exit"));
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
    }

    #[test]
    fn a_new_window_counts_while_older_closes_are_pending() {
        let prompt = QuitPrompt::new();
        assert_eq!(
            prompt.on_close_requested("a", 2, CloseAction::Confirm),
            None
        );
        assert_eq!(
            prompt.on_close_requested("b", 3, CloseAction::Confirm),
            None
        );
        assert_eq!(
            prompt.on_close_requested("c", 3, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
    }

    #[test]
    fn a_prompt_keeps_windows_open_when_another_window_appears() {
        let prompt = QuitPrompt::new();
        assert_eq!(
            prompt.on_close_requested("a", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
        assert_eq!(
            prompt.on_close_requested("b", 2, CloseAction::Confirm),
            Some(KeepOpen::Wait)
        );
        prompt.on_answer(false, || panic!("cancel must not exit"));
        assert_eq!(
            prompt.on_close_requested("b", 2, CloseAction::Confirm),
            None
        );
    }

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
    fn close_request_keeps_the_window_open_unless_quitting() {
        let prompt = QuitPrompt::new();
        assert_eq!(prompt.on_close_requested("a", 1, CloseAction::Quit), None);
        prompt.on_destroyed("a");
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Hide),
            Some(KeepOpen::Hide)
        );
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
        // Closing again while the question is up opens no second one.
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Wait)
        );
    }

    #[test]
    fn cancel_keeps_running_and_the_next_close_asks_again() {
        let prompt = QuitPrompt::new();
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
        let exited = Cell::new(false);
        prompt.on_answer(false, || exited.set(true));
        assert!(!exited.get());
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
    }

    #[test]
    fn quit_anyway_exits() {
        let prompt = QuitPrompt::new();
        assert_eq!(
            prompt.on_close_requested("b", 1, CloseAction::Confirm),
            Some(KeepOpen::Ask)
        );
        let exited = Cell::new(false);
        prompt.on_answer(true, || exited.set(true));
        assert!(exited.get());
    }

    #[test]
    fn runs_in_progress_agrees_in_number() {
        assert_eq!(runs_in_progress(1), "1 run in progress");
        assert_eq!(runs_in_progress(2), "2 runs in progress");
    }
}
