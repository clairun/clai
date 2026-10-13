-- Enum columns held serde-JSON strings ('"running"'); store the bare value,
-- as workspace_tasks.status and assistant_message_queue.status already do.
-- json_extract on a non-JSON value errors, so only rewrite quoted strings.
UPDATE assistant_messages SET role = json_extract(role, '$')
WHERE json_valid(role) AND json_type(role) = 'text';
UPDATE assistant_runs SET status = json_extract(status, '$')
WHERE json_valid(status) AND json_type(status) = 'text';
UPDATE assistant_runs SET "trigger" = json_extract("trigger", '$')
WHERE json_valid("trigger") AND json_type("trigger") = 'text';
UPDATE assistant_tool_calls SET status = json_extract(status, '$')
WHERE json_valid(status) AND json_type(status) = 'text';

-- A CHECK cannot be altered: rebuild. The old CHECKs admit only quoted
-- strings, so the unguarded json_extract below cannot fail. No table
-- references assistant_compactions.
CREATE TABLE assistant_compactions_new (
    id                     TEXT PRIMARY KEY,
    session_id             TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    "trigger"              TEXT NOT NULL CHECK ("trigger" IN ('manual', 'automatic', 'error_recovery')),
    strategy               TEXT NOT NULL CHECK (strategy IN ('local_summary', 'session_rotation_summary')),
    status                 TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    source_from_message_id TEXT REFERENCES assistant_messages(id) ON DELETE SET NULL,
    source_to_message_id   TEXT REFERENCES assistant_messages(id) ON DELETE SET NULL,
    summary_message_id     TEXT REFERENCES assistant_messages(id) ON DELETE SET NULL,
    created_run_id         TEXT REFERENCES assistant_runs(id) ON DELETE SET NULL,
    protocol_id            TEXT NOT NULL,
    model_id               TEXT NOT NULL,
    input_message_count    INTEGER NOT NULL DEFAULT 0,
    created_at             INTEGER NOT NULL,
    completed_at           INTEGER,
    error                  TEXT
);

INSERT INTO assistant_compactions_new
    (id, session_id, "trigger", strategy, status, source_from_message_id, source_to_message_id,
     summary_message_id, created_run_id, protocol_id, model_id, input_message_count, created_at,
     completed_at, error)
SELECT id, session_id, json_extract("trigger", '$'), json_extract(strategy, '$'),
       json_extract(status, '$'), source_from_message_id, source_to_message_id,
       summary_message_id, created_run_id, protocol_id, model_id, input_message_count,
       created_at, completed_at, error
FROM assistant_compactions;

DROP TABLE assistant_compactions;
ALTER TABLE assistant_compactions_new RENAME TO assistant_compactions;

CREATE INDEX idx_assistant_compactions_session_created
    ON assistant_compactions(session_id, created_at DESC);

CREATE INDEX idx_assistant_compactions_session_status
    ON assistant_compactions(session_id, status, created_at DESC);
