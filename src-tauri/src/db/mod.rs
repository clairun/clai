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
}
