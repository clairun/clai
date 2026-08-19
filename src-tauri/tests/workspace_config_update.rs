//! Coverage for `AppState::update_workspace_config{,_at}` — the single
//! read-modify-write path for a workspace `config.json` plus the in-memory
//! index refresh that must stay in step with it.

use std::path::PathBuf;
use std::sync::Barrier;

use clai_lib::{
    workspace_config, AppConfig, AppState, ConfigManager, WorkspaceConfig, WorkspaceIndex,
};
use tempfile::TempDir;

const WORKSPACE_ID: &str = "11111111-1111-4111-8111-111111111111";
const AGENT_ID: &str = "22222222-2222-4222-8222-222222222222";

struct Fixture {
    _temp_dir: TempDir,
    state: AppState,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace_parent = temp_dir.path().join("workspaces");
        let root = workspace_parent.join(WORKSPACE_ID);

        let config = WorkspaceConfig::new(
            WORKSPACE_ID.to_string(),
            "Original title".to_string(),
            1_700_000_000_000,
            AGENT_ID.to_string(),
        );
        workspace_config::save(&root, &config).expect("seed workspace config");

        let mut index = WorkspaceIndex::default();
        index.insert_config(root.clone(), &config);

        let app_config = AppConfig {
            workspace_dirs: vec![workspace_parent],
            ..AppConfig::default()
        };
        let config_manager =
            ConfigManager::new_for_tests(app_config, temp_dir.path().join("config.json"));
        let state = AppState::new_for_tests(config_manager, index).expect("test app state");

        Self {
            _temp_dir: temp_dir,
            state,
            root,
        }
    }

    fn indexed_title(&self) -> String {
        self.state
            .workspace_index
            .read()
            .expect("index lock")
            .locator(WORKSPACE_ID)
            .expect("locator")
            .title
    }

    fn indexed_updated_at(&self) -> i64 {
        self.state
            .workspace_index
            .read()
            .expect("index lock")
            .locator(WORKSPACE_ID)
            .expect("locator")
            .updated_at
    }

    fn on_disk(&self) -> WorkspaceConfig {
        workspace_config::load(&self.root).expect("load workspace config")
    }
}

#[test]
fn update_by_id_writes_disk_and_refreshes_the_index() {
    let fixture = Fixture::new();

    let (returned, saved) = fixture
        .state
        .update_workspace_config(WORKSPACE_ID, |config| {
            config.title = "Renamed".to_string();
            config.updated_at = 1_700_000_001_000;
            Ok(config.agents.len())
        })
        .expect("update");

    assert_eq!(returned, 1, "the closure's value is handed back");
    assert_eq!(saved.title, "Renamed");
    assert_eq!(fixture.on_disk().title, "Renamed");
    assert_eq!(fixture.indexed_title(), "Renamed");
    assert_eq!(fixture.indexed_updated_at(), 1_700_000_001_000);
}

#[test]
fn update_by_root_refreshes_the_index_too() {
    let fixture = Fixture::new();
    let root = fixture.root.clone();

    fixture
        .state
        .update_workspace_config_at(&root, |config| {
            config.title = "Swept".to_string();
            Ok(())
        })
        .expect("update");

    assert_eq!(fixture.on_disk().title, "Swept");
    assert_eq!(fixture.indexed_title(), "Swept");
}

#[test]
fn update_by_id_rejects_an_unknown_workspace() {
    let fixture = Fixture::new();

    let error = fixture
        .state
        .update_workspace_config("does-not-exist", |config| {
            config.title = "Should never be written".to_string();
            Ok(())
        })
        .expect_err("unknown workspace id must fail");

    assert!(
        error.contains("Workspace not found"),
        "unexpected error: {error}"
    );
    assert_eq!(fixture.on_disk().title, "Original title");
    assert_eq!(fixture.indexed_title(), "Original title");
}

#[test]
fn an_aborted_mutation_touches_neither_disk_nor_index() {
    let fixture = Fixture::new();

    let error = fixture
        .state
        .update_workspace_config(WORKSPACE_ID, |config| {
            config.title = "Half-applied".to_string();
            Err::<(), String>("agent not found".to_string())
        })
        .expect_err("aborted mutation must fail");

    assert_eq!(error, "agent not found");
    assert_eq!(fixture.on_disk().title, "Original title");
    assert_eq!(fixture.indexed_title(), "Original title");
}

/// Concurrent writers must not lose an update, and taking the index write
/// lock underneath the config update lock from several threads at once must
/// not deadlock.
///
/// This does not attempt to prove the ordering guarantee — the interleaving
/// that exposes a stale index is not something a scheduler owes us. That half
/// is covered deterministically by
/// `workspace_config::tests::on_saved_runs_before_the_update_lock_is_released`.
#[test]
fn concurrent_updates_keep_disk_and_index_in_step() {
    const THREADS: usize = 4;
    const UPDATES_PER_THREAD: i64 = 40;

    let fixture = Fixture::new();
    let barrier = Barrier::new(THREADS);

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                barrier.wait();
                for _ in 0..UPDATES_PER_THREAD {
                    fixture
                        .state
                        .update_workspace_config(WORKSPACE_ID, |config| {
                            // Read-modify-write: a lost update shows up as a
                            // final counter below the expected total.
                            config.updated_at += 1;
                            Ok(())
                        })
                        .expect("concurrent update");
                }
            });
        }
    });

    let expected = 1_700_000_000_000 + THREADS as i64 * UPDATES_PER_THREAD;
    assert_eq!(
        fixture.on_disk().updated_at,
        expected,
        "no update may be lost on disk"
    );
    assert_eq!(
        fixture.indexed_updated_at(),
        expected,
        "the index must match the saved config"
    );
}
