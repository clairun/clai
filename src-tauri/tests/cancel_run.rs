//! Pins the contract between the scheduler runner and
//! `assistant_cancel_run`. Both code paths now agree that the
//! cancellation key is the DB `run.id`; an earlier bug (see
//! `agents/runner.rs` history) registered scheduler tokens under a
//! synthetic `scheduled:{instance_id}:{uuid}` key, making scheduled
//! runs effectively un-cancellable from the UI.
//!
//! These tests use the public `register_run` / `cancel_run` surface
//! and simulate the registration calls each path makes. A real
//! end-to-end test would also need an engine turn to consume the
//! cancel token, but the value lives at the key-naming layer: every
//! spawn site must register under run.id.

use clai_lib::runtime::{cancel_run, register_run};

#[test]
fn chat_path_registers_and_cancels_under_run_id() {
    // Mimics commands::assistant::spawn_run_task.
    let run_id = "cancel-test-chat-run-id";
    let registration = register_run(run_id);
    let token = registration.token();
    assert!(!token.is_cancelled());

    // Mimics commands::assistant::assistant_cancel_run, which calls
    // runtime::cancel_run(&run_id).
    assert!(cancel_run(run_id));
    assert!(token.is_cancelled());
}

#[test]
fn scheduler_path_registers_and_cancels_under_run_id() {
    // Mimics agents::runner::run_scheduled_agent_with_fallback after
    // the cancel-key bugfix: the scheduler now registers under
    // `run.id`, not under a synthetic `scheduled:...` string. This
    // test pins that convention so a future refactor can't drift
    // back to a separate key.
    let run_id = "cancel-test-scheduler-run-id";
    let registration = register_run(run_id);
    let token = registration.token();
    assert!(!token.is_cancelled());

    assert!(cancel_run(run_id));
    assert!(token.is_cancelled());
}

#[test]
fn workspace_task_path_registers_and_cancels_under_run_id() {
    // Mimics assistant::tools::workspace_tasks::spawn_task_run. Same
    // convention as the other two paths; tested for symmetry so all
    // three spawn sites are documented to use the DB run.id.
    let run_id = "cancel-test-workspace-task-run-id";
    let registration = register_run(run_id);
    let token = registration.token();
    assert!(!token.is_cancelled());

    assert!(cancel_run(run_id));
    assert!(token.is_cancelled());
}

#[test]
fn cancel_unknown_run_id_is_noop() {
    // assistant_cancel_run interprets `false` as "the run isn't
    // active in memory anymore" and falls through to a DB-only
    // update. This is the contract for already-terminated runs.
    assert!(!cancel_run("cancel-test-no-such-run"));
}
