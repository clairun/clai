//! `live_run_count` decides whether quitting asks first. It counts the
//! process-global registry, so it lives in its own test binary: no other
//! test registers runs while this one counts.

use clai_lib::runtime::{live_run_count, register_run, LiveRunKey};

#[test]
fn live_run_count_follows_registrations() {
    let key = |session: &str| LiveRunKey::new("count-test-ws", session);
    assert_eq!(live_run_count(), 0);

    let first = register_run(key("a"), "run-a");
    let second = register_run(key("b"), "run-b");
    assert_eq!(live_run_count(), 2);

    // A newer run of the same session replaces the older one; dropping the
    // older registration later leaves the newer one counted.
    let newer = register_run(key("a"), "run-a2");
    assert_eq!(live_run_count(), 2);
    drop(first);
    assert_eq!(live_run_count(), 2);

    drop(second);
    assert_eq!(live_run_count(), 1);
    drop(newer);
    assert_eq!(live_run_count(), 0);
}
