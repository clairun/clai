-- Durable per-session signals: the control plane that says which input still
-- needs a run. Content signals point at the message row written in the same
-- transaction; `assistant_message_queue` becomes `user_message` signals.

CREATE TABLE session_signals (
    id                TEXT PRIMARY KEY,
    target_session_id TEXT NOT NULL REFERENCES assistant_sessions(id) ON DELETE CASCADE,
    kind              TEXT NOT NULL,
    message_id        TEXT REFERENCES assistant_messages(id) ON DELETE SET NULL,
    origin_json       TEXT NOT NULL DEFAULT '{"kind":"user"}',
    delivery_key      TEXT,
    connection_id     TEXT,                          -- user_message only; NULL = resolve at claim
    status            TEXT NOT NULL CHECK (status IN ('pending','claimed','consumed')),
    claimed_run_id    TEXT REFERENCES assistant_runs(id) ON DELETE SET NULL,
    consumed_run_id   TEXT REFERENCES assistant_runs(id) ON DELETE SET NULL,
    created_at        INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_session_signals_delivery_key
    ON session_signals(delivery_key) WHERE delivery_key IS NOT NULL;
CREATE INDEX idx_session_signals_target_open
    ON session_signals(target_session_id, created_at) WHERE status IN ('pending','claimed');
CREATE INDEX idx_session_signals_claimed_run ON session_signals(claimed_run_id) WHERE claimed_run_id IS NOT NULL;
CREATE INDEX idx_session_signals_origin_window ON session_signals(created_at, kind);

INSERT INTO session_signals (id, target_session_id, kind, message_id, origin_json, connection_id,
                             status, consumed_run_id, created_at)
SELECT 'q:' || message_id, session_id, 'user_message', message_id, '{"kind":"user"}', connection_id,
       CASE status WHEN 'pending' THEN 'pending' ELSE 'consumed' END,
       delivered_run_id, queued_at
FROM assistant_message_queue;
DROP TABLE assistant_message_queue;

-- One active run per session: close duplicates, then enforce. The index is
-- the claim arbiter for dispatcher::kick; its predicate must list exactly
-- repository::ACTIVE_RUN_STATUSES.
UPDATE assistant_runs
SET status = 'failed', error = COALESCE(error, 'Superseded by session single-run migration'),
    completed_at = COALESCE(completed_at, started_at)
WHERE status IN ('queued', 'running', 'waiting_for_tool')
  AND id NOT IN (SELECT id FROM (
      SELECT id, ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY started_at DESC, id DESC) AS rn
      FROM assistant_runs WHERE status IN ('queued', 'running', 'waiting_for_tool')) WHERE rn = 1);
CREATE UNIQUE INDEX idx_assistant_runs_one_active_per_session
    ON assistant_runs(session_id) WHERE status IN ('queued', 'running', 'waiting_for_tool');
