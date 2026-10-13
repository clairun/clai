//! The live runs of this process, one per session: the cancel token and the
//! mid-run wake of each. The database's one-active-run-per-session index
//! guarantees at most one registration per key at a time; the generation
//! keeps a late drop of an older guard from removing a newer entry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveRunKey {
    pub workspace_id: String,
    pub session_id: String,
}

impl LiveRunKey {
    pub fn new(workspace_id: &str, session_id: &str) -> Self {
        Self {
            workspace_id: workspace_id.to_string(),
            session_id: session_id.to_string(),
        }
    }
}

#[derive(Clone)]
struct LiveRun {
    run_id: String,
    token: CancellationToken,
    /// Woken when new content signals are pending for the session.
    midrun: Arc<Notify>,
    generation: u64,
}

type LiveRuns = HashMap<LiveRunKey, LiveRun>;

static LIVE_RUNS: OnceLock<Mutex<LiveRuns>> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn live_runs() -> &'static Mutex<LiveRuns> {
    LIVE_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct RunRegistration {
    key: LiveRunKey,
    token: CancellationToken,
    generation: u64,
}

impl RunRegistration {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for RunRegistration {
    fn drop(&mut self) {
        let mut live = live_runs().lock().unwrap();
        if live
            .get(&self.key)
            .is_some_and(|run| run.generation == self.generation)
        {
            live.remove(&self.key);
        }
    }
}

pub fn register_run(key: LiveRunKey, run_id: &str) -> RunRegistration {
    let token = CancellationToken::new();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    live_runs().lock().unwrap().insert(
        key.clone(),
        LiveRun {
            run_id: run_id.to_string(),
            token: token.clone(),
            midrun: Arc::new(Notify::new()),
            generation,
        },
    );
    RunRegistration {
        key,
        token,
        generation,
    }
}

/// Cancels the session's live run, if any.
pub fn cancel_session(key: &LiveRunKey) -> bool {
    let token = live_runs()
        .lock()
        .unwrap()
        .get(key)
        .map(|run| run.token.clone());
    token.map(|token| token.cancel()).is_some()
}

pub fn cancel_run(run_id: &str) -> bool {
    let token = live_runs()
        .lock()
        .unwrap()
        .values()
        .find(|run| run.run_id == run_id)
        .map(|run| run.token.clone());
    token.map(|token| token.cancel()).is_some()
}

/// Tells the session's live run that content signals are pending. The wake
/// is kept until the run waits for it, so none is lost.
pub fn notify_midrun(key: &LiveRunKey) -> bool {
    let midrun = live_runs()
        .lock()
        .unwrap()
        .get(key)
        .map(|run| run.midrun.clone());
    midrun.map(|midrun| midrun.notify_one()).is_some()
}

/// The mid-run wake of `run_id`, while it is the session's live run.
pub fn midrun(key: &LiveRunKey, run_id: &str) -> Option<Arc<Notify>> {
    live_runs()
        .lock()
        .unwrap()
        .get(key)
        .filter(|run| run.run_id == run_id)
        .map(|run| run.midrun.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique session ids per test: the map is process-global and tests run
    // in parallel.
    fn key(session_id: &str) -> LiveRunKey {
        LiveRunKey::new("runtime-test-ws", session_id)
    }

    #[test]
    fn cancel_session_and_cancel_run_reach_the_token() {
        let registration = register_run(key("runtime-cancel-session"), "run-a");
        let token = registration.token();
        assert!(cancel_session(&key("runtime-cancel-session")));
        assert!(token.is_cancelled());

        let registration = register_run(key("runtime-cancel-run"), "run-b");
        let token = registration.token();
        assert!(cancel_run("run-b"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn unknown_or_unregistered_runs_are_not_found() {
        assert!(!cancel_run("runtime-test-nonexistent-id"));
        assert!(!cancel_session(&key("runtime-nonexistent")));

        let registration = register_run(key("runtime-unregister"), "run-c");
        drop(registration);
        assert!(!cancel_session(&key("runtime-unregister")));
        assert!(!cancel_run("run-c"));
    }

    #[test]
    fn dropping_an_older_registration_keeps_the_newer_one() {
        let first = register_run(key("runtime-reregister"), "run-1");
        let second = register_run(key("runtime-reregister"), "run-2");
        drop(first);
        assert!(midrun(&key("runtime-reregister"), "run-2").is_some());
        assert!(midrun(&key("runtime-reregister"), "run-1").is_none());
        drop(second);
        assert!(!cancel_session(&key("runtime-reregister")));
    }

    #[tokio::test]
    async fn midrun_channel_delivers_without_poll() {
        let key = key("runtime-midrun");
        let _registration = register_run(key.clone(), "run-m");
        let wake = midrun(&key, "run-m").expect("the live run has a mid-run wake");

        // Notified before the run waits: the wake is stored, not lost.
        assert!(notify_midrun(&key));
        tokio::time::timeout(std::time::Duration::from_millis(100), wake.notified())
            .await
            .expect("a pending wake resolves at once, without a poll interval");

        assert!(!notify_midrun(&LiveRunKey::new("runtime-test-ws", "other")));
    }
}
