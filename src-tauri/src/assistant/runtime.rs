use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct ActiveRunEntry {
    token: CancellationToken,
    generation: u64,
}

type ActiveRuns = HashMap<String, ActiveRunEntry>;

static ACTIVE_RUNS: OnceLock<Mutex<ActiveRuns>> = OnceLock::new();
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub struct RunRegistration {
    run_id: String,
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
        let mut active = active_runs().lock().unwrap();
        if active
            .get(&self.run_id)
            .is_some_and(|entry| entry.generation == self.generation)
        {
            active.remove(&self.run_id);
        }
    }
}

fn active_runs() -> &'static Mutex<ActiveRuns> {
    ACTIVE_RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_run(run_id: &str) -> RunRegistration {
    let token = CancellationToken::new();
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    active_runs().lock().unwrap().insert(
        run_id.to_string(),
        ActiveRunEntry {
            token: token.clone(),
            generation,
        },
    );
    RunRegistration {
        run_id: run_id.to_string(),
        token,
        generation,
    }
}

pub fn cancel_run(run_id: &str) -> bool {
    if let Some(token) = active_runs()
        .lock()
        .unwrap()
        .get(run_id)
        .map(|entry| entry.token.clone())
    {
        token.cancel();
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use unique ids per test so parallel execution of `cargo test`
    // doesn't share state through the static `ACTIVE_RUNS` map.
    // The unique-id approach is simpler than gating on a global mutex
    // and matches how the engine assigns run ids in production.

    #[test]
    fn register_then_cancel_propagates_to_token() {
        let id = "runtime-test-register-then-cancel";
        let registration = register_run(id);
        let token = registration.token();
        assert!(!token.is_cancelled(), "fresh token must start uncancelled");

        let was_found = cancel_run(id);
        assert!(was_found, "cancel_run must report success for a known id");
        assert!(
            token.is_cancelled(),
            "the original token handle must observe the cancel"
        );
    }

    #[test]
    fn cancel_unknown_run_returns_false() {
        // Defends the assistant_cancel_run code path: when a run isn't
        // active anymore (e.g. it terminated before the user clicked
        // Stop), cancel_run must return false so the caller falls back
        // to marking the DB row Cancelled directly.
        let was_found = cancel_run("runtime-test-nonexistent-id");
        assert!(!was_found, "cancel_run must return false for unknown id");
    }

    #[test]
    fn unregister_removes_from_active_set() {
        let id = "runtime-test-unregister-removes";
        let registration = register_run(id);
        drop(registration);

        let was_found = cancel_run(id);
        assert!(!was_found, "unregistered ids must no longer be cancellable");
    }

    #[test]
    fn re_register_same_id_replaces_token_handle() {
        // Defensive: the engine's spawn_run_task and the scheduler
        // runner both register under run.id. If somehow the same
        // run.id were reused (it shouldn't be — UUIDs), the latest
        // registration wins. Pin the behavior so a future refactor
        // doesn't accidentally start de-duping or asserting.
        let id = "runtime-test-double-register";
        let first_registration = register_run(id);
        let first = first_registration.token();
        let second_registration = register_run(id);
        let second = second_registration.token();

        // Cancelling now should signal the second (current) token.
        assert!(cancel_run(id));
        assert!(second.is_cancelled());
        // The first token is orphaned — no longer in the map, so
        // cancel_run never reaches it.
        assert!(!first.is_cancelled());

        drop(first_registration);
        assert!(
            cancel_run(id),
            "dropping an older guard must not unregister the newer token"
        );

        drop(second_registration);
        assert!(!cancel_run(id));
    }
}
