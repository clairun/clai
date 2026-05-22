//! Database module for workspace state persistence.
//!
//! Uses SQLite via SQLx to store tabs and commands state.
//! This enables full workspace restoration on app restart and stores
//! assistant session state for the app-owned assistant runtime.

use sqlx::{sqlite::SqlitePoolOptions, Pool, Row, Sqlite};
use std::path::PathBuf;

/// Type alias for SQLite connection pool
pub type DbPool = Pool<Sqlite>;

/// Get the database file path in the app's data directory
fn get_db_path() -> Result<PathBuf, String> {
    let data_dir = dirs::data_dir().ok_or_else(|| "Could not find data directory".to_string())?;

    let app_dir = data_dir.join("clai");

    // Create directory if it doesn't exist
    std::fs::create_dir_all(&app_dir)
        .map_err(|e| format!("Failed to create app data directory: {}", e))?;

    Ok(app_dir.join("workspace.db"))
}

fn get_legacy_config_path() -> Result<PathBuf, String> {
    let config_dir =
        dirs::config_dir().ok_or_else(|| "Could not find config directory".to_string())?;
    Ok(config_dir.join("clai").join("config.json"))
}

fn read_legacy_default_model() -> Result<Option<String>, String> {
    let path = get_legacy_config_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "Failed to read legacy config file {}: {}",
            path.display(),
            e
        )
    })?;
    let json: serde_json::Value = serde_json::from_str(&contents).map_err(|e| {
        format!(
            "Failed to parse legacy config file {}: {}",
            path.display(),
            e
        )
    })?;

    Ok(json
        .get("assistant_default_model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string))
}

async fn table_exists(pool: &DbPool, table_name: &str) -> Result<bool, String> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
    )
    .bind(table_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to check table {}: {}", table_name, e))?
    .is_some();

    Ok(exists)
}

async fn column_exists(pool: &DbPool, table_name: &str, column_name: &str) -> Result<bool, String> {
    let pragma = format!("PRAGMA table_info({})", table_name);
    let rows = sqlx::query(&pragma)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to inspect schema for {}: {}", table_name, e))?;

    Ok(rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column_name))
}

async fn foreign_key_targets(pool: &DbPool, table_name: &str) -> Result<Vec<String>, String> {
    if !table_exists(pool, table_name).await? {
        return Ok(Vec::new());
    }

    let pragma = format!("PRAGMA foreign_key_list({})", table_name);
    let rows = sqlx::query(&pragma)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("Failed to inspect foreign keys for {}: {}", table_name, e))?;

    Ok(rows
        .iter()
        .map(|row| row.get::<String, _>("table"))
        .collect())
}

async fn table_references_target(
    pool: &DbPool,
    table_name: &str,
    expected_target: &str,
) -> Result<bool, String> {
    Ok(foreign_key_targets(pool, table_name)
        .await?
        .iter()
        .any(|target| target == expected_target))
}

async fn drop_table_if_exists(pool: &DbPool, table_name: &str) -> Result<(), String> {
    if table_exists(pool, table_name).await? {
        sqlx::query(&format!("DROP TABLE {}", table_name))
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to drop legacy table {}: {}", table_name, e))?;
    }

    Ok(())
}

async fn migrate_provider_connections(pool: &DbPool) -> Result<(), String> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS provider_connections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            auth_mode TEXT NOT NULL,
            base_url TEXT,
            secret_ref TEXT NOT NULL,
            model_id TEXT NOT NULL,
            account_label TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create provider_connections table: {}", e))?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_provider_connections_enabled
        ON provider_connections(enabled, updated_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create provider_connections index: {}", e))?;

    if !table_exists(pool, "provider_sessions").await? {
        return Ok(());
    }

    let existing_connections =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_connections")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("Failed to count provider connections: {}", e))?;

    if existing_connections > 0 {
        return Ok(());
    }

    let legacy_rows = sqlx::query(
        r#"
        SELECT provider_id, auth_mode, base_url, secret_ref, account_label, created_at, updated_at
        FROM provider_sessions
        ORDER BY updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to load provider_sessions for migration: {}", e))?;

    if legacy_rows.is_empty() {
        return Ok(());
    }

    let legacy_model = read_legacy_default_model()?
        .ok_or_else(|| "Cannot migrate provider sessions: assistant_default_model is missing from legacy config".to_string())?;

    for row in legacy_rows {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            r#"
            INSERT INTO provider_connections
                (id, name, provider_id, auth_mode, base_url, secret_ref, model_id, account_label, enabled, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(row.get::<String, _>("provider_id"))
        .bind(row.get::<String, _>("provider_id"))
        .bind(row.get::<String, _>("auth_mode"))
        .bind(row.get::<Option<String>, _>("base_url"))
        .bind(row.get::<String, _>("secret_ref"))
        .bind(&legacy_model)
        .bind(row.get::<Option<String>, _>("account_label"))
        .bind(row.get::<i64, _>("created_at"))
        .bind(row.get::<i64, _>("updated_at"))
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to migrate provider session to provider connection: {}", e))?;
    }

    Ok(())
}

async fn migrate_assistant_sessions(pool: &DbPool) -> Result<(), String> {
    if !table_exists(pool, "assistant_sessions").await? {
        sqlx::query(
            r#"
            CREATE TABLE assistant_sessions (
                id TEXT PRIMARY KEY,
                tab_id TEXT REFERENCES tabs(id) ON DELETE SET NULL,
                kind TEXT NOT NULL,
                title TEXT,
                context_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create assistant_sessions table: {}", e))?;
    } else if column_exists(pool, "assistant_sessions", "provider_id").await?
        || column_exists(pool, "assistant_sessions", "model_id").await?
    {
        sqlx::query("ALTER TABLE assistant_sessions RENAME TO assistant_sessions_legacy")
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to rename legacy assistant_sessions table: {}", e))?;

        sqlx::query(
            r#"
            CREATE TABLE assistant_sessions (
                id TEXT PRIMARY KEY,
                tab_id TEXT REFERENCES tabs(id) ON DELETE SET NULL,
                kind TEXT NOT NULL,
                title TEXT,
                context_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create migrated assistant_sessions table: {}", e))?;

        sqlx::query(
            r#"
            INSERT INTO assistant_sessions (id, tab_id, kind, title, context_json, created_at, updated_at)
            SELECT id, tab_id, kind, title, context_json, created_at, updated_at
            FROM assistant_sessions_legacy
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to migrate assistant_sessions rows: {}", e))?;

        sqlx::query("DROP TABLE assistant_sessions_legacy")
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to drop legacy assistant_sessions table: {}", e))?;
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_assistant_sessions_tab
        ON assistant_sessions(tab_id)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant_sessions tab index: {}", e))?;

    Ok(())
}

async fn migrate_assistant_runs(pool: &DbPool) -> Result<(), String> {
    if !table_exists(pool, "assistant_runs").await? {
        sqlx::query(
            r#"
            CREATE TABLE assistant_runs (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                trigger TEXT NOT NULL,
                connection_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                usage_json TEXT,
                error TEXT,
                notices_json TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create assistant_runs table: {}", e))?;
    } else {
        let needs_rebuild =
            !table_references_target(pool, "assistant_runs", "assistant_sessions").await?;

        if needs_rebuild {
            sqlx::query("ALTER TABLE assistant_runs RENAME TO assistant_runs_legacy")
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to rename legacy assistant_runs table: {}", e))?;

            sqlx::query(
                r#"
                CREATE TABLE assistant_runs (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                    status TEXT NOT NULL,
                    trigger TEXT NOT NULL,
                    connection_id TEXT NOT NULL,
                    provider_id TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    usage_json TEXT,
                    error TEXT,
                    notices_json TEXT,
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER
                )
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to recreate assistant_runs table: {}", e))?;

            let legacy_connection_expr = if column_exists(
                pool,
                "assistant_runs_legacy",
                "connection_id",
            )
            .await?
            {
                "COALESCE(connection_id, (SELECT pc.id FROM provider_connections pc WHERE pc.provider_id = assistant_runs_legacy.provider_id ORDER BY pc.created_at ASC LIMIT 1), '')"
            } else {
                "COALESCE((SELECT pc.id FROM provider_connections pc WHERE pc.provider_id = assistant_runs_legacy.provider_id ORDER BY pc.created_at ASC LIMIT 1), '')"
            };
            let legacy_notices_expr =
                if column_exists(pool, "assistant_runs_legacy", "notices_json").await? {
                    "notices_json"
                } else {
                    "NULL"
                };

            let copy_sql = format!(
                r#"
                INSERT INTO assistant_runs (
                    id, session_id, status, trigger, connection_id, provider_id, model_id,
                    usage_json, error, notices_json, started_at, completed_at
                )
                SELECT
                    id, session_id, status, trigger, {connection_expr}, provider_id, model_id,
                    usage_json, error, {notices_expr}, started_at, completed_at
                FROM assistant_runs_legacy
                "#,
                connection_expr = legacy_connection_expr,
                notices_expr = legacy_notices_expr,
            );

            sqlx::query(&copy_sql)
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to migrate assistant_runs rows: {}", e))?;

            sqlx::query("DROP TABLE assistant_runs_legacy")
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to drop legacy assistant_runs table: {}", e))?;
        } else {
            if !column_exists(pool, "assistant_runs", "connection_id").await? {
                sqlx::query("ALTER TABLE assistant_runs ADD COLUMN connection_id TEXT")
                    .execute(pool)
                    .await
                    .map_err(|e| format!("Failed to add assistant_runs.connection_id: {}", e))?;
            }

            if !column_exists(pool, "assistant_runs", "notices_json").await? {
                sqlx::query("ALTER TABLE assistant_runs ADD COLUMN notices_json TEXT")
                    .execute(pool)
                    .await
                    .map_err(|e| format!("Failed to add assistant_runs.notices_json: {}", e))?;
            }

            sqlx::query(
                r#"
                UPDATE assistant_runs
                SET connection_id = (
                    SELECT pc.id
                    FROM provider_connections pc
                    WHERE pc.provider_id = assistant_runs.provider_id
                    ORDER BY pc.created_at ASC
                    LIMIT 1
                )
                WHERE connection_id IS NULL
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to backfill assistant_runs.connection_id: {}", e))?;
        }
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_assistant_runs_session
        ON assistant_runs(session_id, started_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant_runs session index: {}", e))?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_assistant_runs_connection
        ON assistant_runs(connection_id, started_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant_runs connection index: {}", e))?;

    Ok(())
}

async fn migrate_assistant_messages(pool: &DbPool) -> Result<(), String> {
    if !table_exists(pool, "assistant_messages").await? {
        sqlx::query(
            r#"
            CREATE TABLE assistant_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content_json TEXT NOT NULL,
                provider_metadata_json TEXT,
                created_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create assistant_messages table: {}", e))?;
    } else if !table_references_target(pool, "assistant_messages", "assistant_sessions").await? {
        sqlx::query("ALTER TABLE assistant_messages RENAME TO assistant_messages_legacy")
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to rename legacy assistant_messages table: {}", e))?;

        sqlx::query(
            r#"
            CREATE TABLE assistant_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content_json TEXT NOT NULL,
                provider_metadata_json TEXT,
                created_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to recreate assistant_messages table: {}", e))?;

        sqlx::query(
            r#"
            INSERT INTO assistant_messages (id, session_id, role, content_json, provider_metadata_json, created_at)
            SELECT id, session_id, role, content_json, provider_metadata_json, created_at
            FROM assistant_messages_legacy
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to migrate assistant_messages rows: {}", e))?;

        sqlx::query("DROP TABLE assistant_messages_legacy")
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to drop legacy assistant_messages table: {}", e))?;
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_assistant_messages_session
        ON assistant_messages(session_id, created_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant_messages index: {}", e))?;

    Ok(())
}

async fn migrate_assistant_tool_calls(pool: &DbPool) -> Result<(), String> {
    if !table_exists(pool, "assistant_tool_calls").await? {
        sqlx::query(
            r#"
            CREATE TABLE assistant_tool_calls (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                tool_name TEXT NOT NULL,
                params_json TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                error TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
        )
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to create assistant_tool_calls table: {}", e))?;
    } else {
        let fk_targets = foreign_key_targets(pool, "assistant_tool_calls").await?;
        let needs_rebuild = !fk_targets.iter().any(|target| target == "assistant_runs")
            || !fk_targets
                .iter()
                .any(|target| target == "assistant_sessions");

        if needs_rebuild {
            sqlx::query("ALTER TABLE assistant_tool_calls RENAME TO assistant_tool_calls_legacy")
                .execute(pool)
                .await
                .map_err(|e| {
                    format!("Failed to rename legacy assistant_tool_calls table: {}", e)
                })?;

            sqlx::query(
                r#"
                CREATE TABLE assistant_tool_calls (
                    id TEXT PRIMARY KEY,
                    run_id TEXT NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
                    session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                    tool_name TEXT NOT NULL,
                    params_json TEXT NOT NULL,
                    status TEXT NOT NULL,
                    result_json TEXT,
                    error TEXT,
                    started_at INTEGER NOT NULL,
                    completed_at INTEGER
                )
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to recreate assistant_tool_calls table: {}", e))?;

            sqlx::query(
                r#"
                INSERT INTO assistant_tool_calls (
                    id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
                )
                SELECT
                    id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at
                FROM assistant_tool_calls_legacy
                "#,
            )
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to migrate assistant_tool_calls rows: {}", e))?;

            sqlx::query("DROP TABLE assistant_tool_calls_legacy")
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to drop legacy assistant_tool_calls table: {}", e))?;
        }
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_assistant_tool_calls_run
        ON assistant_tool_calls(run_id, started_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create assistant_tool_calls index: {}", e))?;

    Ok(())
}

async fn migrate_workspaces(pool: &DbPool) -> Result<(), String> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS workspaces (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL DEFAULT 'general',
            title TEXT,
            preferred_provider_connection_id TEXT,
            default_workspace_agent_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspaces table: {}", e))?;

    // Add preferred_provider_connection_id column if missing (added after initial migration)
    if !column_exists(pool, "workspaces", "preferred_provider_connection_id").await? {
        sqlx::query("ALTER TABLE workspaces ADD COLUMN preferred_provider_connection_id TEXT")
            .execute(pool)
            .await
            .map_err(|e| {
                format!(
                    "Failed to add workspaces.preferred_provider_connection_id: {}",
                    e
                )
            })?;
    }

    // Add default_workspace_agent_id column if missing (agent fleet workspace rosters)
    if !column_exists(pool, "workspaces", "default_workspace_agent_id").await? {
        sqlx::query("ALTER TABLE workspaces ADD COLUMN default_workspace_agent_id TEXT")
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to add workspaces.default_workspace_agent_id: {}", e))?;
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspaces_updated
        ON workspaces(updated_at DESC)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspaces index: {}", e))?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS workspace_agents (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            agent_definition_id TEXT NOT NULL,
            display_name TEXT,
            role TEXT NOT NULL DEFAULT 'member',
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_agents table: {}", e))?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspace_agents_workspace
        ON workspace_agents(workspace_id)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_agents workspace index: {}", e))?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspace_agents_definition
        ON workspace_agents(agent_definition_id)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_agents definition index: {}", e))?;

    // Phase 1.1 of workspace-local-agents migration: additive columns.
    //
    // We add inline copies of the agent fields that previously lived only in
    // `ClaiConfig.agents` (the global catalog) reachable via the
    // `agent_definition_id` foreign key. After all consumers have been
    // migrated (phase 1.4 / 1.5), the legacy columns will be dropped
    // (phase 1.7). For now both shapes coexist; nothing reads the new
    // columns yet, so behavior is unchanged.
    //
    // See `docs/BUNDLED_BUILDING_BLOCKS_RFC.md` (commit 1 sub-phases).
    for (column, ddl) in [
        ("name", "name TEXT NOT NULL DEFAULT ''"),
        ("description", "description TEXT NOT NULL DEFAULT ''"),
        (
            "selected_skill_ids",
            "selected_skill_ids TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "selected_mcp_server_ids",
            "selected_mcp_server_ids TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "provider_connection_ids",
            "provider_connection_ids TEXT NOT NULL DEFAULT '[]'",
        ),
        ("execution", "execution TEXT NOT NULL DEFAULT '{}'"),
        ("exposed_tools", "exposed_tools TEXT NOT NULL DEFAULT '[]'"),
        (
            "schedule_enabled",
            "schedule_enabled INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "interval_minutes",
            "interval_minutes INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "schedule_paused",
            "schedule_paused INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !column_exists(pool, "workspace_agents", column).await? {
            sqlx::query(&format!("ALTER TABLE workspace_agents ADD COLUMN {}", ddl))
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to add workspace_agents.{} column: {}", column, e))?;
        }
    }

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS workspace_tasks (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            created_by_workspace_agent_id TEXT,
            assigned_to_workspace_agent_id TEXT NOT NULL,
            assigned_agent_definition_id TEXT NOT NULL,
            title TEXT NOT NULL,
            instructions TEXT NOT NULL,
            status TEXT NOT NULL,
            result_summary TEXT,
            result_json TEXT,
            error TEXT,
            session_id TEXT,
            run_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            completed_at INTEGER,
            attention_acknowledged_at INTEGER,
            user_response TEXT,
            user_response_at INTEGER
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_tasks table: {}", e))?;

    for (column, column_type) in [
        ("attention_acknowledged_at", "INTEGER"),
        ("user_response", "TEXT"),
        ("user_response_at", "INTEGER"),
    ] {
        if !column_exists(pool, "workspace_tasks", column).await? {
            sqlx::query(&format!(
                "ALTER TABLE workspace_tasks ADD COLUMN {} {}",
                column, column_type
            ))
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to add workspace_tasks.{}: {}", column, e))?;
        }
    }

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspace_tasks_workspace
        ON workspace_tasks(workspace_id, updated_at DESC)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_tasks workspace index: {}", e))?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspace_tasks_assigned_agent
        ON workspace_tasks(assigned_to_workspace_agent_id, updated_at DESC)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| {
        format!(
            "Failed to create workspace_tasks assigned-agent index: {}",
            e
        )
    })?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_workspace_tasks_status
        ON workspace_tasks(workspace_id, status, updated_at DESC)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_tasks status index: {}", e))?;

    // Phase 1.3 of workspace-local-agents migration: nuke pre-Phase-1.2 rows.
    //
    // Any workspace_agents row whose `name` is still the empty-string default
    // was inserted before Phase 1.2 added inline data, which means it has no
    // local copy of the agent's prompt/skills/etc. Per the RFC §5.9 "nuke
    // existing state" stance, drop these rows. Their workspaces will appear
    // without a manager; the user can re-assign or re-create.
    //
    // Then clear any dangling `default_workspace_agent_id` pointers that
    // referenced the deleted rows.
    sqlx::query("DELETE FROM workspace_agents WHERE name = ''")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to nuke pre-Phase-1.2 workspace_agents rows: {}", e))?;

    sqlx::query(
        r#"
        UPDATE workspaces
        SET default_workspace_agent_id = NULL
        WHERE default_workspace_agent_id IS NOT NULL
          AND default_workspace_agent_id NOT IN (SELECT id FROM workspace_agents)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| {
        format!(
            "Failed to clear dangling workspace default-agent pointers: {}",
            e
        )
    })?;

    Ok(())
}

/// Initialize the database connection pool and run migrations
pub async fn init_db() -> Result<DbPool, String> {
    let db_path = get_db_path()?;
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

    tracing::info!("Initializing database at: {}", db_path.display());

    // Create connection pool
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .map_err(|e| format!("Failed to connect to database: {}", e))?;

    // Run schema migrations
    run_migrations(&pool).await?;

    tracing::info!("Database initialized successfully");
    Ok(pool)
}

/// Run database schema migrations
async fn run_migrations(pool: &DbPool) -> Result<(), String> {
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to disable foreign keys during migrations: {}", e))?;

    // Create tabs table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tabs (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            root_tile TEXT NOT NULL,
            context TEXT NOT NULL,
            position INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create tabs table: {}", e))?;

    // Create commands table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS commands (
            id TEXT PRIMARY KEY,
            tab_id TEXT NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
            tile_id TEXT NOT NULL,
            type TEXT NOT NULL,
            args TEXT,
            state TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create commands table: {}", e))?;

    // Create index on commands.tab_id
    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_commands_tab ON commands(tab_id)
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create commands index: {}", e))?;

    // Create workspace_meta table for active tab tracking
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS workspace_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to create workspace_meta table: {}", e))?;

    migrate_provider_connections(pool).await?;
    migrate_assistant_sessions(pool).await?;

    migrate_assistant_messages(pool).await?;

    migrate_assistant_runs(pool).await?;

    migrate_assistant_tool_calls(pool).await?;

    migrate_workspaces(pool).await?;

    split_workspace_agent_prefixes(pool).await?;

    canonicalize_legacy_tool_names(pool).await?;

    sweep_orphaned_running_state(pool).await?;

    for legacy_table in [
        "assistant_sessions_legacy",
        "assistant_messages_legacy",
        "assistant_runs_legacy",
        "assistant_tool_calls_legacy",
        "provider_sessions",
    ] {
        drop_table_if_exists(pool, legacy_table).await?;
    }

    // Enable foreign keys
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to enable foreign keys: {}", e))?;

    Ok(())
}

/// Startup sweep: marks `workspace_tasks` rows stuck in `running` as
/// failed. They're orphans from a previous app process that died, was
/// killed by a rebuild, or otherwise didn't finalize. Without this,
/// the rows pile up as forever-"RUNNING" in the UI (the agent session
/// that owned them no longer exists, nothing can resolve them).
///
/// `assistant_runs` and `assistant_tool_calls` are NOT touched here —
/// `assistant::repository::recover_stale_runs` already handles those
/// at startup, and its SQL uses the correct JSON-quoted enum format
/// the column actually stores (e.g. `'"running"'`, not `'running'`).
/// Duplicating that work here with bare-string status values just
/// produced a no-op UPDATE (matched zero rows) and added connection-
/// pool contention during startup.
async fn sweep_orphaned_running_state(pool: &DbPool) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();

    let tasks = sqlx::query(
        r#"
        UPDATE workspace_tasks
        SET status = 'failed',
            error = COALESCE(error, 'task interrupted by app restart'),
            updated_at = ?,
            completed_at = ?
        WHERE status = 'running'
        "#,
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to sweep orphaned workspace_tasks: {}", e))?;
    if tasks.rows_affected() > 0 {
        tracing::info!(
            "Marked {} workspace_tasks as failed (orphaned 'running' state from previous app session)",
            tasks.rows_affected()
        );
    }
    Ok(())
}

/// Idempotent migration: canonicalizes built-in tool names from the
/// legacy dotted form (`fs.list`, `bash.exec`, ...) to the underscore
/// form OpenAI-compatible providers require (`fs_list`, `bash_exec`).
///
/// Touches two surfaces:
/// 1. `assistant_tool_calls.tool_name` — a direct column. Plain SQL UPDATE.
/// 2. `assistant_messages.content_json` — JSON blobs containing
///    `tool_use` parts with a `tool_name` field. Read each row, walk the
///    `content` array, rewrite any object with a known legacy
///    `tool_name`, write back if anything changed.
///
/// Both passes are idempotent: re-running over already-canonicalized
/// data is a no-op.
async fn canonicalize_legacy_tool_names(pool: &DbPool) -> Result<(), String> {
    // 1. Direct column update.
    let direct = sqlx::query(
        r#"
        UPDATE assistant_tool_calls
        SET tool_name = REPLACE(tool_name, '.', '_')
        WHERE tool_name LIKE 'fs.%'
           OR tool_name LIKE 'bash.%'
           OR tool_name LIKE 'web.%'
           OR tool_name LIKE 'workspace.%'
           OR tool_name LIKE 'agent.%'
        "#,
    )
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to canonicalize assistant_tool_calls names: {}", e))?;
    if direct.rows_affected() > 0 {
        tracing::info!(
            "Canonicalized {} assistant_tool_calls.tool_name rows (`.` → `_`)",
            direct.rows_affected()
        );
    }

    // 2. JSON blob rewrite for content parts.
    let rows: Vec<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT id, content_json FROM assistant_messages")
            .fetch_all(pool)
            .await
            .map_err(|e| {
                format!(
                    "Failed to read assistant_messages for tool-name migration: {}",
                    e
                )
            })?;

    let mut blob_updates = 0_u32;
    for (id, content_json) in rows {
        let mut content: serde_json::Value = match serde_json::from_str(&content_json) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed rows; the rest of the app handles them too
        };
        let mut changed = false;
        if let Some(arr) = content.as_array_mut() {
            for part in arr.iter_mut() {
                if let Some(obj) = part.as_object_mut() {
                    if let Some(serde_json::Value::String(name)) = obj.get_mut("tool_name") {
                        if is_legacy_dotted_tool_name(name) {
                            *name = name.replacen('.', "_", 1);
                            changed = true;
                        }
                    }
                }
            }
        }
        if !changed {
            continue;
        }
        let new_json = serde_json::to_string(&content).map_err(|e| {
            format!(
                "Failed to re-serialize content_json for assistant_messages {}: {}",
                id, e
            )
        })?;
        sqlx::query("UPDATE assistant_messages SET content_json = ? WHERE id = ?")
            .bind(new_json)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update assistant_messages {}: {}", id, e))?;
        blob_updates += 1;
    }
    if blob_updates > 0 {
        tracing::info!(
            "Canonicalized tool_name fields in {} assistant_messages content blobs",
            blob_updates
        );
    }

    Ok(())
}

fn is_legacy_dotted_tool_name(name: &str) -> bool {
    const LEGACY_PREFIXES: &[&str] = &["fs.", "bash.", "web.", "workspace.", "agent."];
    LEGACY_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Idempotent migration: walks every `workspace_agents` row and runs each
/// entry of `execution.shell.allowedCommandPrefixes` and
/// `blockedCommandPrefixes` through the command splitter, replacing the
/// entry with the textual content of its resulting segments. Dedupe is
/// applied per-list, preserving insertion order.
///
/// Existing entries that contain shell separators (`|`, `&&`, etc.) become
/// dead under the new per-segment matcher (which never sees a separator
/// inside a segment), so this migration repairs them.
///
/// Re-running this migration is a no-op: a single-segment entry splits
/// back to itself.
async fn split_workspace_agent_prefixes(pool: &DbPool) -> Result<(), String> {
    use crate::assistant::tools::command_splitter::split_command;

    let rows: Vec<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT id, execution FROM workspace_agents")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("Failed to read workspace_agents for prefix split: {}", e))?;

    let mut migrated = 0_u32;
    for (id, execution_json) in rows {
        let mut execution: serde_json::Value = match serde_json::from_str(&execution_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Skipping workspace_agent {} during prefix migration: invalid JSON ({})",
                    id,
                    e
                );
                continue;
            }
        };

        let Some(shell) = execution.get_mut("shell").and_then(|v| v.as_object_mut()) else {
            continue;
        };

        let changed_allow = split_prefix_array(shell, "allowedCommandPrefixes", split_command);
        let changed_block = split_prefix_array(shell, "blockedCommandPrefixes", split_command);
        if !changed_allow && !changed_block {
            continue;
        }

        let new_json = serde_json::to_string(&execution).map_err(|e| {
            format!(
                "Failed to re-serialize execution for workspace_agent {}: {}",
                id, e
            )
        })?;
        sqlx::query("UPDATE workspace_agents SET execution = ? WHERE id = ?")
            .bind(new_json)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to update workspace_agents {}: {}", id, e))?;
        migrated += 1;
    }

    if migrated > 0 {
        tracing::info!(
            "Split workspace_agents allow/block lists for {} rows (separators no longer matter)",
            migrated
        );
    }

    Ok(())
}

fn split_prefix_array<F>(
    shell: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    splitter: F,
) -> bool
where
    F: Fn(&str) -> Vec<crate::assistant::tools::command_splitter::Segment>,
{
    let Some(entries) = shell.get(key).and_then(|v| v.as_array()).cloned() else {
        return false;
    };

    let mut out: Vec<String> = Vec::with_capacity(entries.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed = false;

    for entry in &entries {
        let Some(text) = entry.as_str() else {
            changed = true; // drop non-string entries
            continue;
        };
        let segs = splitter(text);
        if segs.is_empty() {
            changed = true; // entry was whitespace-only, drop
            continue;
        }
        let pieces: Vec<String> = segs.iter().map(|s| s.text().to_string()).collect();
        if pieces.len() == 1 && pieces[0] == text {
            // No semantic split; preserve unless duplicate.
            if seen.insert(pieces[0].clone()) {
                out.push(pieces.into_iter().next().unwrap());
            } else {
                changed = true;
            }
            continue;
        }
        // Either multi-segment or text was normalized (whitespace).
        changed = true;
        for piece in pieces {
            if seen.insert(piece.clone()) {
                out.push(piece);
            }
        }
    }

    if changed {
        shell.insert(
            key.to_string(),
            serde_json::Value::Array(out.into_iter().map(serde_json::Value::String).collect()),
        );
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn create_test_pool() -> DbPool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn repairs_assistant_tables_after_session_rename_migration() {
        let pool = create_test_pool().await;

        sqlx::query(
            r#"
            CREATE TABLE tabs (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                root_tile TEXT NOT NULL,
                context TEXT NOT NULL,
                position INTEGER,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE assistant_sessions (
                id TEXT PRIMARY KEY,
                tab_id TEXT REFERENCES tabs(id) ON DELETE SET NULL,
                kind TEXT NOT NULL,
                title TEXT,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                context_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE assistant_runs (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                trigger TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                usage_json TEXT,
                error TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE assistant_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content_json TEXT NOT NULL,
                provider_metadata_json TEXT,
                created_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            CREATE TABLE assistant_tool_calls (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
                tool_name TEXT NOT NULL,
                params_json TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT,
                error TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO assistant_sessions
                (id, tab_id, kind, title, provider_id, model_id, context_json, created_at, updated_at)
            VALUES
                ('session-old', NULL, '"chat"', 'Legacy session', 'openai', 'gpt-4o', '{}', 1, 1)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO assistant_runs
                (id, session_id, status, trigger, provider_id, model_id, usage_json, error, started_at, completed_at)
            VALUES
                ('run-old', 'session-old', '"completed"', '"user"', 'openai', 'gpt-4o', NULL, NULL, 1, 2)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO assistant_messages
                (id, session_id, role, content_json, provider_metadata_json, created_at)
            VALUES
                ('message-old', 'session-old', '"user"', '[]', NULL, 1)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO assistant_tool_calls
                (id, run_id, session_id, tool_name, params_json, status, result_json, error, started_at, completed_at)
            VALUES
                ('tool-old', 'run-old', 'session-old', 'echo', '{}', '"completed"', NULL, NULL, 1, 2)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        assert!(table_exists(&pool, "assistant_sessions").await.unwrap());
        assert!(table_exists(&pool, "assistant_runs").await.unwrap());
        assert!(table_exists(&pool, "assistant_messages").await.unwrap());
        assert!(table_exists(&pool, "assistant_tool_calls").await.unwrap());
        assert!(!table_exists(&pool, "assistant_sessions_legacy")
            .await
            .unwrap());
        assert!(!table_exists(&pool, "assistant_runs_legacy").await.unwrap());
        assert!(!table_exists(&pool, "assistant_messages_legacy")
            .await
            .unwrap());
        assert!(!table_exists(&pool, "assistant_tool_calls_legacy")
            .await
            .unwrap());

        assert!(
            table_references_target(&pool, "assistant_runs", "assistant_sessions")
                .await
                .unwrap()
        );
        assert!(
            table_references_target(&pool, "assistant_messages", "assistant_sessions")
                .await
                .unwrap()
        );

        let tool_call_targets = foreign_key_targets(&pool, "assistant_tool_calls")
            .await
            .unwrap();
        assert!(tool_call_targets
            .iter()
            .any(|target| target == "assistant_runs"));
        assert!(tool_call_targets
            .iter()
            .any(|target| target == "assistant_sessions"));

        sqlx::query(
            r#"
            INSERT INTO assistant_sessions
                (id, tab_id, kind, title, context_json, created_at, updated_at)
            VALUES
                ('session-new', NULL, '"chat"', 'New session', '{}', 3, 3)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"
            INSERT INTO assistant_runs
                (id, session_id, status, trigger, connection_id, provider_id, model_id, usage_json, error, notices_json, started_at, completed_at)
            VALUES
                ('run-new', 'session-new', '"running"', '"user"', 'connection-1', 'openai', 'gpt-4o', NULL, NULL, NULL, 4, NULL)
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // -------------------------------------------------------------------
    // split_workspace_agent_prefixes migration
    // -------------------------------------------------------------------

    /// Bare minimum schema for the prefix-split migration to run against.
    /// Only the columns it reads/writes are needed.
    async fn create_workspace_agents_for_split_test(pool: &DbPool) {
        sqlx::query(
            r#"
            CREATE TABLE workspace_agents (
                id TEXT PRIMARY KEY,
                execution TEXT NOT NULL DEFAULT '{}'
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_agent_with_execution(pool: &DbPool, id: &str, execution_json: &str) {
        sqlx::query("INSERT INTO workspace_agents (id, execution) VALUES (?, ?)")
            .bind(id)
            .bind(execution_json)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn read_agent_execution(pool: &DbPool, id: &str) -> serde_json::Value {
        let json: String =
            sqlx::query_scalar("SELECT execution FROM workspace_agents WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[tokio::test]
    async fn split_migration_breaks_apart_pipe_containing_entries() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git log | head","kubectl get"],"blockedCommandPrefixes":[]}}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec = read_agent_execution(&pool, "a").await;
        let allowed = exec
            .pointer("/shell/allowedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap();
        let strings: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(strings, vec!["git log", "head", "kubectl get"]);
    }

    #[tokio::test]
    async fn split_migration_is_idempotent() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git log | head"],"blockedCommandPrefixes":[]}}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();
        let first_pass = read_agent_execution(&pool, "a").await;

        split_workspace_agent_prefixes(&pool).await.unwrap();
        let second_pass = read_agent_execution(&pool, "a").await;

        assert_eq!(first_pass, second_pass);
    }

    #[tokio::test]
    async fn split_migration_dedupes_results() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git log; git log","git log"],"blockedCommandPrefixes":[]}}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec = read_agent_execution(&pool, "a").await;
        let allowed = exec
            .pointer("/shell/allowedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap();
        let strings: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(strings, vec!["git log"]);
    }

    #[tokio::test]
    async fn split_migration_handles_blocklist_too() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":[],"blockedCommandPrefixes":["rm; sudo"]}}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec = read_agent_execution(&pool, "a").await;
        let blocked = exec
            .pointer("/shell/blockedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap();
        let strings: Vec<&str> = blocked.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(strings, vec!["rm", "sudo"]);
    }

    #[tokio::test]
    async fn split_migration_skips_malformed_json_without_failing() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(&pool, "broken", "{ not valid json").await;
        insert_agent_with_execution(
            &pool,
            "ok",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["a; b"],"blockedCommandPrefixes":[]}}"#,
        )
        .await;

        // Should not error.
        split_workspace_agent_prefixes(&pool).await.unwrap();

        // The valid row still got migrated.
        let exec = read_agent_execution(&pool, "ok").await;
        let allowed = exec
            .pointer("/shell/allowedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap();
        let strings: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(strings, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn split_migration_no_op_when_no_separators() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        let original = r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git status","kubectl logs"],"blockedCommandPrefixes":["rm"]}}"#;
        insert_agent_with_execution(&pool, "a", original).await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec_json: String =
            sqlx::query_scalar("SELECT execution FROM workspace_agents WHERE id = 'a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let exec: serde_json::Value = serde_json::from_str(&exec_json).unwrap();
        let allowed: Vec<&str> = exec
            .pointer("/shell/allowedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(allowed, vec!["git status", "kubectl logs"]);
    }

    #[tokio::test]
    async fn split_migration_preserves_unknown_fields() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        // Include a future-unknown field alongside the standard shape.
        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git log | head"],"blockedCommandPrefixes":[],"futureField":42},"unknownTopLevel":"keep me"}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec = read_agent_execution(&pool, "a").await;
        assert_eq!(
            exec.pointer("/shell/futureField"),
            Some(&serde_json::json!(42))
        );
        assert_eq!(
            exec.pointer("/unknownTopLevel"),
            Some(&serde_json::Value::String("keep me".to_string()))
        );
    }

    #[tokio::test]
    async fn split_migration_drops_non_string_entries() {
        let pool = create_test_pool().await;
        create_workspace_agents_for_split_test(&pool).await;

        insert_agent_with_execution(
            &pool,
            "a",
            r#"{"shell":{"mode":"restricted","allowedCommandPrefixes":["git status",42,null],"blockedCommandPrefixes":[]}}"#,
        )
        .await;

        split_workspace_agent_prefixes(&pool).await.unwrap();

        let exec = read_agent_execution(&pool, "a").await;
        let allowed: Vec<&str> = exec
            .pointer("/shell/allowedCommandPrefixes")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(allowed, vec!["git status"]);
    }

    // -------------------------------------------------------------------
    // canonicalize_legacy_tool_names migration
    // -------------------------------------------------------------------

    async fn create_legacy_tool_name_schema(pool: &DbPool) {
        sqlx::query(
            r#"
            CREATE TABLE assistant_tool_calls (
                id TEXT PRIMARY KEY,
                tool_name TEXT NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE assistant_messages (
                id TEXT PRIMARY KEY,
                content_json TEXT NOT NULL
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn canonicalize_rewrites_dotted_tool_calls_column() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        sqlx::query("INSERT INTO assistant_tool_calls VALUES ('tc1', 'bash.exec')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO assistant_tool_calls VALUES ('tc2', 'fs.read')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO assistant_tool_calls VALUES ('tc3', 'already_canonical')")
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let names: Vec<String> =
            sqlx::query_scalar("SELECT tool_name FROM assistant_tool_calls ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            names,
            vec![
                "bash_exec".to_string(),
                "fs_read".to_string(),
                "already_canonical".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn canonicalize_rewrites_tool_name_in_content_json() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        let legacy = r#"[
            {"type":"text","text":"hi"},
            {"type":"tool_use","tool_call_id":"a","tool_name":"bash.exec","arguments":{}},
            {"type":"tool_use","tool_call_id":"b","tool_name":"workspace.listAgents","arguments":{}},
            {"type":"tool_use","tool_call_id":"c","tool_name":"external_mcp.tool","arguments":{}}
        ]"#;
        sqlx::query("INSERT INTO assistant_messages VALUES ('m1', ?)")
            .bind(legacy)
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let updated: String =
            sqlx::query_scalar("SELECT content_json FROM assistant_messages WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(updated.contains("\"tool_name\":\"bash_exec\""));
        assert!(updated.contains("\"tool_name\":\"workspace_listAgents\""));
        // External MCP names (no known legacy prefix) must be left alone.
        assert!(updated.contains("\"tool_name\":\"external_mcp.tool\""));
    }

    #[tokio::test]
    async fn canonicalize_is_idempotent() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        sqlx::query("INSERT INTO assistant_tool_calls VALUES ('tc1', 'bash.exec')")
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();
        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let name: String =
            sqlx::query_scalar("SELECT tool_name FROM assistant_tool_calls WHERE id = 'tc1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "bash_exec");
    }

    // -------------------------------------------------------------------
    // is_legacy_dotted_tool_name (pure helper)
    // -------------------------------------------------------------------

    #[test]
    fn is_legacy_dotted_recognizes_all_known_prefixes() {
        assert!(is_legacy_dotted_tool_name("fs.list"));
        assert!(is_legacy_dotted_tool_name("bash.exec"));
        assert!(is_legacy_dotted_tool_name("web.fetch"));
        assert!(is_legacy_dotted_tool_name("workspace.listAgents"));
        assert!(is_legacy_dotted_tool_name("agent.spawn"));
    }

    #[test]
    fn is_legacy_dotted_rejects_canonical_underscore_form() {
        assert!(!is_legacy_dotted_tool_name("fs_list"));
        assert!(!is_legacy_dotted_tool_name("bash_exec"));
        assert!(!is_legacy_dotted_tool_name("workspace_listAgents"));
    }

    #[test]
    fn is_legacy_dotted_rejects_external_mcp_names() {
        // External MCP tool names use a `.` but are NOT in the legacy
        // built-in prefix list. They must be left alone.
        assert!(!is_legacy_dotted_tool_name("external_mcp.tool"));
        assert!(!is_legacy_dotted_tool_name("github.create_issue"));
        assert!(!is_legacy_dotted_tool_name("notion.search"));
    }

    #[test]
    fn is_legacy_dotted_rejects_empty_and_dotless_strings() {
        assert!(!is_legacy_dotted_tool_name(""));
        assert!(!is_legacy_dotted_tool_name("nope"));
        // Bare prefix without trailing dot does not match.
        assert!(!is_legacy_dotted_tool_name("fs"));
        assert!(!is_legacy_dotted_tool_name("bash"));
    }

    #[test]
    fn is_legacy_dotted_requires_dot_terminated_prefix() {
        // Strings that start with one of the prefix letters minus the
        // dot must not match — the matcher requires the trailing `.`.
        assert!(!is_legacy_dotted_tool_name("fsx.read"));
        assert!(!is_legacy_dotted_tool_name("webhook.post"));
        // But the exact prefix + dot + name does match, even with an
        // empty suffix.
        assert!(is_legacy_dotted_tool_name("fs."));
        assert!(is_legacy_dotted_tool_name("bash.x"));
    }

    // -------------------------------------------------------------------
    // table_exists / column_exists / foreign_key_targets /
    // drop_table_if_exists
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn table_exists_returns_false_for_missing_table() {
        let pool = create_test_pool().await;
        assert!(!table_exists(&pool, "nope").await.unwrap());
    }

    #[tokio::test]
    async fn table_exists_returns_true_after_create() {
        let pool = create_test_pool().await;
        sqlx::query("CREATE TABLE foo (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        assert!(table_exists(&pool, "foo").await.unwrap());
    }

    #[tokio::test]
    async fn column_exists_distinguishes_present_and_absent_columns() {
        let pool = create_test_pool().await;
        sqlx::query("CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        assert!(column_exists(&pool, "foo", "id").await.unwrap());
        assert!(column_exists(&pool, "foo", "name").await.unwrap());
        assert!(!column_exists(&pool, "foo", "missing").await.unwrap());
    }

    #[tokio::test]
    async fn foreign_key_targets_lists_referenced_tables() {
        let pool = create_test_pool().await;
        sqlx::query("CREATE TABLE parent (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let targets = foreign_key_targets(&pool, "child").await.unwrap();
        assert_eq!(targets, vec!["parent".to_string()]);
    }

    #[tokio::test]
    async fn foreign_key_targets_returns_empty_for_missing_table() {
        let pool = create_test_pool().await;
        // Documented behavior: an absent table returns an empty Vec,
        // not an error.
        let targets = foreign_key_targets(&pool, "nope").await.unwrap();
        assert!(targets.is_empty());
    }

    #[tokio::test]
    async fn drop_table_if_exists_is_a_no_op_when_absent() {
        let pool = create_test_pool().await;
        // Must not error even though the table does not exist.
        drop_table_if_exists(&pool, "nope").await.unwrap();
        assert!(!table_exists(&pool, "nope").await.unwrap());
    }

    #[tokio::test]
    async fn drop_table_if_exists_drops_present_table() {
        let pool = create_test_pool().await;
        sqlx::query("CREATE TABLE foo (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        assert!(table_exists(&pool, "foo").await.unwrap());
        drop_table_if_exists(&pool, "foo").await.unwrap();
        assert!(!table_exists(&pool, "foo").await.unwrap());
    }

    // -------------------------------------------------------------------
    // sweep_orphaned_running_state
    // -------------------------------------------------------------------

    /// Minimal `workspace_tasks` schema for sweep tests. Only the
    /// columns the sweep reads/writes (status, error, updated_at,
    /// completed_at) plus the PK — kept small so the test does not
    /// have to track unrelated schema changes elsewhere.
    async fn create_workspace_tasks_for_sweep_test(pool: &DbPool) {
        sqlx::query(
            r#"
            CREATE TABLE workspace_tasks (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                error TEXT,
                updated_at INTEGER NOT NULL,
                completed_at INTEGER
            )
            "#,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_sweep_task(pool: &DbPool, id: &str, status: &str, error: Option<&str>) {
        sqlx::query(
            "INSERT INTO workspace_tasks (id, status, error, updated_at, completed_at) \
             VALUES (?, ?, ?, 1, NULL)",
        )
        .bind(id)
        .bind(status)
        .bind(error)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn sweep_marks_running_rows_as_failed() {
        let pool = create_test_pool().await;
        create_workspace_tasks_for_sweep_test(&pool).await;
        insert_sweep_task(&pool, "t1", "running", None).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let row: (String, Option<String>, i64, Option<i64>) = sqlx::query_as(
            "SELECT status, error, updated_at, completed_at \
             FROM workspace_tasks WHERE id = 't1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("task interrupted by app restart"));
        // updated_at and completed_at are stamped from chrono::Utc::now()
        // — we only check they were re-stamped (not the placeholder 1)
        // and that completed_at is no longer NULL.
        assert!(row.2 > 1, "updated_at must be re-stamped: {}", row.2);
        let completed_at = row.3.expect("completed_at must be set after sweep");
        assert!(completed_at > 1);
    }

    #[tokio::test]
    async fn sweep_preserves_existing_error_via_coalesce() {
        let pool = create_test_pool().await;
        create_workspace_tasks_for_sweep_test(&pool).await;
        insert_sweep_task(&pool, "t1", "running", Some("custom failure reason")).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let error: Option<String> =
            sqlx::query_scalar("SELECT error FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            error.as_deref(),
            Some("custom failure reason"),
            "COALESCE must keep the existing error",
        );
    }

    #[tokio::test]
    async fn sweep_leaves_non_running_rows_untouched() {
        let pool = create_test_pool().await;
        create_workspace_tasks_for_sweep_test(&pool).await;
        insert_sweep_task(&pool, "done", "completed", None).await;
        insert_sweep_task(&pool, "fail", "failed", Some("original error")).await;
        insert_sweep_task(&pool, "pending", "pending", None).await;

        sweep_orphaned_running_state(&pool).await.unwrap();

        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT id, status, error, updated_at \
             FROM workspace_tasks ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        for (id, status, _err, updated_at) in &rows {
            assert_ne!(status, "running", "row {} should not be running", id);
            assert_eq!(
                *updated_at, 1,
                "non-running row {} must not be re-stamped",
                id
            );
        }
    }

    #[tokio::test]
    async fn sweep_is_idempotent_after_no_running_rows_remain() {
        let pool = create_test_pool().await;
        create_workspace_tasks_for_sweep_test(&pool).await;
        insert_sweep_task(&pool, "t1", "running", None).await;

        sweep_orphaned_running_state(&pool).await.unwrap();
        let after_first: (String, i64) =
            sqlx::query_as("SELECT status, updated_at FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after_first.0, "failed");

        // Second sweep: no rows are 'running' anymore, so nothing should
        // change — updated_at must stay at its first-sweep value.
        sweep_orphaned_running_state(&pool).await.unwrap();
        let after_second: (String, i64) =
            sqlx::query_as("SELECT status, updated_at FROM workspace_tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after_second.0, "failed");
        assert_eq!(
            after_first.1, after_second.1,
            "updated_at must not move on a second sweep",
        );
    }

    #[tokio::test]
    async fn sweep_handles_empty_table() {
        let pool = create_test_pool().await;
        create_workspace_tasks_for_sweep_test(&pool).await;
        // Must not error.
        sweep_orphaned_running_state(&pool).await.unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_tasks")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    // -------------------------------------------------------------------
    // canonicalize_legacy_tool_names — additional JSON edge cases
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn canonicalize_skips_messages_with_non_array_content() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        // Non-array content (a single object) — the helper inspects
        // `as_array_mut` and must not crash or alter this shape.
        let json = r#"{"type":"text","text":"hi","tool_name":"bash.exec"}"#;
        sqlx::query("INSERT INTO assistant_messages VALUES ('m1', ?)")
            .bind(json)
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let updated: String =
            sqlx::query_scalar("SELECT content_json FROM assistant_messages WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        // Untouched — non-array content is skipped.
        assert!(updated.contains("\"tool_name\":\"bash.exec\""));
    }

    #[tokio::test]
    async fn canonicalize_skips_parts_without_tool_name_field() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        let json = r#"[
            {"type":"text","text":"hi"},
            {"type":"thinking","text":"thought"}
        ]"#;
        sqlx::query("INSERT INTO assistant_messages VALUES ('m1', ?)")
            .bind(json)
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let updated: String =
            sqlx::query_scalar("SELECT content_json FROM assistant_messages WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        // The parts have no tool_name; round-trip should be unchanged.
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        let arr = parsed.as_array().expect("content remains an array");
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().all(|v| v.get("tool_name").is_none()));
    }

    #[tokio::test]
    async fn canonicalize_only_rewrites_first_dot_in_legacy_prefix() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        // A legacy-prefixed name with a second `.` after the slot
        // (`bash.exec.advanced`). The helper uses
        // `replacen('.', "_", 1)`, so only the leading dot is rewritten.
        let json = r#"[{"type":"tool_use","tool_call_id":"a","tool_name":"bash.exec.advanced"}]"#;
        sqlx::query("INSERT INTO assistant_messages VALUES ('m1', ?)")
            .bind(json)
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let updated: String =
            sqlx::query_scalar("SELECT content_json FROM assistant_messages WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(updated.contains("\"tool_name\":\"bash_exec.advanced\""));
    }

    #[tokio::test]
    async fn canonicalize_handles_mixed_legacy_and_canonical_in_same_row() {
        let pool = create_test_pool().await;
        create_legacy_tool_name_schema(&pool).await;
        let json = r#"[
            {"type":"tool_use","tool_call_id":"a","tool_name":"fs.list"},
            {"type":"tool_use","tool_call_id":"b","tool_name":"fs_read"},
            {"type":"tool_use","tool_call_id":"c","tool_name":"workspace.assignTask"}
        ]"#;
        sqlx::query("INSERT INTO assistant_messages VALUES ('m1', ?)")
            .bind(json)
            .execute(&pool)
            .await
            .unwrap();

        canonicalize_legacy_tool_names(&pool).await.unwrap();

        let updated: String =
            sqlx::query_scalar("SELECT content_json FROM assistant_messages WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&updated).unwrap();
        let names: Vec<&str> = parsed
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.get("tool_name").and_then(|n| n.as_str()))
            .collect();
        assert_eq!(
            names,
            vec!["fs_list", "fs_read", "workspace_assignTask"],
            "legacy names rewritten, already-canonical names left alone",
        );
    }
}
