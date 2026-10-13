//! Pins the contract between the run spawn sites and `assistant_cancel_run`:
//! a run is registered under its session and is cancellable by its DB
//! `run.id`. An earlier bug registered scheduler tokens under a synthetic
//! key, making scheduled runs un-cancellable from the UI.

use clai_lib::runtime::{cancel_run, register_run, LiveRunKey};

#[test]
fn a_run_registered_under_its_session_cancels_by_run_id() {
    let registration = register_run(
        LiveRunKey::new("cancel-test-ws", "cancel-test-session"),
        "cancel-test-run-id",
    );
    let token = registration.token();
    assert!(!token.is_cancelled());

    assert!(cancel_run("cancel-test-run-id"));
    assert!(token.is_cancelled());
}

#[test]
fn cancel_unknown_run_id_is_noop() {
    // assistant_cancel_run interprets `false` as "the run isn't
    // active in memory anymore" and falls through to a DB-only
    // update. This is the contract for already-terminated runs.
    assert!(!cancel_run("cancel-test-no-such-run"));
}
