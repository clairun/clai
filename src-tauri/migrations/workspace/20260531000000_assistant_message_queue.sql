-- User messages typed while a session already has an active run.
--
-- The assistant_messages row remains the canonical chat transcript. This
-- side-table tracks which of those user rows still need a provider turn, and
-- which run eventually consumed them.

CREATE TABLE assistant_message_queue (
    message_id       TEXT PRIMARY KEY REFERENCES assistant_messages(id) ON DELETE CASCADE,
    session_id       TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    connection_id    TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('pending', 'delivered')),
    queued_at        INTEGER NOT NULL,
    delivered_run_id TEXT REFERENCES assistant_runs(id) ON DELETE SET NULL,
    delivered_at     INTEGER
);

CREATE INDEX idx_assistant_message_queue_session_status
    ON assistant_message_queue(session_id, status, queued_at);

CREATE INDEX idx_assistant_message_queue_delivered_run
    ON assistant_message_queue(delivered_run_id, queued_at);
