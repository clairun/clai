use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use clai_lib::{
    workspace_agent_runtime_description, workspace_config, AppConfig, AppState, ConfigManager,
    SkillSourceConfig, WorkspaceConfig, WorkspaceIndex,
};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct ClaiHomeGuard {
    previous: Option<OsString>,
}

impl ClaiHomeGuard {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("CLAI_HOME");
        std::env::set_var("CLAI_HOME", path);
        Self { previous }
    }
}

impl Drop for ClaiHomeGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var("CLAI_HOME", previous);
        } else {
            std::env::remove_var("CLAI_HOME");
        }
    }
}

struct PromptBuildFixture {
    _env_lock: MutexGuard<'static, ()>,
    _clai_home: ClaiHomeGuard,
    _temp_dir: TempDir,
    state: AppState,
    workspace_id: String,
    agent_id: String,
}

impl PromptBuildFixture {
    fn new(agent_description: &str, selected_skills: Vec<workspace_config::SkillRef>) -> Self {
        let env_lock = ENV_LOCK.lock().expect("CLAI_HOME test lock poisoned");
        let temp_dir = TempDir::new().expect("temp dir");
        let clai_home_path = temp_dir.path().join("clai-home");
        let clai_home = ClaiHomeGuard::set(&clai_home_path);
        let workspace_parent = temp_dir.path().join("workspaces");
        let default_skills_root = clai_home_path
            .join("cache")
            .join("skill-sources")
            .join("clai-skills");

        let skill_dir = default_skills_root.join("prompt-build-skill");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: \"Prompt build fixture.\"\n---\n# Prompt Build Skill\nFAKE_SKILL_SENTINEL\n",
        )
        .expect("skill file");

        let default_source = SkillSourceConfig::new_git(
            "CLAI Skills".to_string(),
            "https://github.com/clairun/clai-skills.git".to_string(),
            None,
            Some(default_skills_root.display().to_string()),
        );
        let app_config = AppConfig {
            workspace_dirs: vec![workspace_parent.clone()],
            skill_sources: vec![default_source],
            ..AppConfig::default()
        };
        let config_manager =
            ConfigManager::new_for_tests(app_config, clai_home_path.join("config.json"));

        let workspace_id = "11111111-1111-4111-8111-111111111111".to_string();
        let agent_id = "22222222-2222-4222-8222-222222222222".to_string();
        let workspace_root = workspace_parent.join(&workspace_id);
        let mut workspace_config = WorkspaceConfig::new(
            workspace_id.clone(),
            "Prompt Build Workspace".to_string(),
            1_700_000_000_000,
            agent_id.clone(),
        );
        let manager = workspace_config
            .agents
            .iter_mut()
            .find(|agent| agent.id == agent_id)
            .expect("manager agent");
        manager.description = agent_description.to_string();
        manager.selected_skills = selected_skills;
        workspace_config::save(&workspace_root, &workspace_config).expect("workspace config");

        let mut workspace_index = WorkspaceIndex::default();
        workspace_index.insert_config(workspace_root, &workspace_config);
        let state =
            AppState::new_for_tests(config_manager, workspace_index).expect("test app state");

        Self {
            _env_lock: env_lock,
            _clai_home: clai_home,
            _temp_dir: temp_dir,
            state,
            workspace_id,
            agent_id,
        }
    }
}

#[test]
fn workspace_agent_runtime_description_includes_selected_skill_content() {
    let fixture = PromptBuildFixture::new(
        "Base manager instructions.",
        vec![workspace_config::SkillRef::Bundled {
            slug: "prompt-build-skill".to_string(),
        }],
    );

    let prompt = workspace_agent_runtime_description(
        &fixture.state,
        &fixture.workspace_id,
        &fixture.agent_id,
    )
    .expect("runtime description");

    assert!(prompt.contains("Base manager instructions."));
    assert!(prompt.contains("## Selected Skills"));
    assert!(prompt.contains("Prompt Build Skill"));
    assert!(prompt.contains("FAKE_SKILL_SENTINEL"));
}

#[test]
fn workspace_agent_runtime_description_without_skills_is_raw_description() {
    let fixture = PromptBuildFixture::new("Raw manager instructions.", Vec::new());

    let prompt = workspace_agent_runtime_description(
        &fixture.state,
        &fixture.workspace_id,
        &fixture.agent_id,
    )
    .expect("runtime description");

    assert_eq!(prompt, "Raw manager instructions.");
}
