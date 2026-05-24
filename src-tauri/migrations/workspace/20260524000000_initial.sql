-- Initial schema for per-workspace SQLite databases (<root>/.clai/data.sqlite).
--
-- Each workspace owns its own database. There is no `workspace_id` column
-- on any table — workspace identity is implicit by which DB you connected
-- to. Add a new numbered file to this directory whenever the workspace
-- schema needs to change — sqlx::migrate! runs at every workspace open
-- (startup eager fan-out + lazy on-access), so workspaces idle across app
-- updates catch up the next time they are touched.

CREATE TABLE assistant_sessions (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,
    title         TEXT,
    context_json  TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE assistant_messages (
    id                     TEXT PRIMARY KEY,
    session_id             TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    role                   TEXT NOT NULL,
    content_json           TEXT NOT NULL,
    provider_metadata_json TEXT,
    created_at             INTEGER NOT NULL
);

CREATE INDEX idx_assistant_messages_session
    ON assistant_messages(session_id, created_at);

CREATE TABLE assistant_runs (
    id             TEXT PRIMARY KEY,
    session_id     TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    status         TEXT NOT NULL,
    trigger        TEXT NOT NULL,
    connection_id  TEXT NOT NULL,
    provider_id    TEXT NOT NULL,
    model_id       TEXT NOT NULL,
    usage_json     TEXT,
    error          TEXT,
    notices_json   TEXT,
    started_at     INTEGER NOT NULL,
    completed_at   INTEGER
);

CREATE INDEX idx_assistant_runs_session
    ON assistant_runs(session_id, started_at);

CREATE TABLE assistant_tool_calls (
    id            TEXT PRIMARY KEY,
    run_id        TEXT NOT NULL REFERENCES assistant_runs(id) ON DELETE CASCADE,
    session_id    TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    tool_name     TEXT NOT NULL,
    params_json   TEXT NOT NULL,
    status        TEXT NOT NULL,
    result_json   TEXT,
    error         TEXT,
    started_at    INTEGER NOT NULL,
    completed_at  INTEGER
);

CREATE INDEX idx_assistant_tool_calls_run
    ON assistant_tool_calls(run_id, started_at);

CREATE TABLE workspace_tasks (
    id                             TEXT PRIMARY KEY,
    created_by_workspace_agent_id  TEXT,
    assigned_to_workspace_agent_id TEXT NOT NULL,
    assigned_agent_definition_id   TEXT NOT NULL,
    title                          TEXT NOT NULL,
    instructions                   TEXT NOT NULL,
    status                         TEXT NOT NULL,
    result_summary                 TEXT,
    result_json                    TEXT,
    error                          TEXT,
    session_id                     TEXT,
    run_id                         TEXT,
    created_at                     INTEGER NOT NULL,
    updated_at                     INTEGER NOT NULL,
    completed_at                   INTEGER,
    attention_acknowledged_at      INTEGER,
    user_response                  TEXT,
    user_response_at               INTEGER
);

CREATE INDEX idx_workspace_tasks_assigned_agent
    ON workspace_tasks(assigned_to_workspace_agent_id, updated_at DESC);

CREATE INDEX idx_workspace_tasks_status
    ON workspace_tasks(status, updated_at DESC);
