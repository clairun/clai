-- Who wrote a message and which run wrote it; task lineage, cycles and the
-- failure reason that replaces the `blocked` status.

ALTER TABLE assistant_messages ADD COLUMN origin_json TEXT NOT NULL DEFAULT '{"kind":"user"}';
ALTER TABLE assistant_messages ADD COLUMN run_id TEXT REFERENCES assistant_runs(id) ON DELETE SET NULL;
CREATE INDEX idx_assistant_messages_run ON assistant_messages(run_id, created_at);

-- The first user row of a task session is the creator's instruction. The
-- roster lives outside SQLite, so the snapshot falls back to the raw id.
UPDATE assistant_messages
SET origin_json = (
    SELECT json_object('kind', 'agent', 'agent', json_object(
               'workspaceAgentId', task.created_by_workspace_agent_id,
               'displayName', task.created_by_workspace_agent_id,
               'isDefault', json('false')))
    FROM workspace_tasks AS task
    WHERE task.session_id = assistant_messages.session_id)
WHERE id IN (
    SELECT (SELECT msg.id FROM assistant_messages AS msg
            WHERE msg.session_id = task.session_id AND msg.role = 'user'
            ORDER BY msg.created_at, msg.id LIMIT 1)
    FROM workspace_tasks AS task
    WHERE task.session_id IS NOT NULL AND task.created_by_workspace_agent_id IS NOT NULL);

ALTER TABLE workspace_tasks ADD COLUMN parent_session_id TEXT;
ALTER TABLE workspace_tasks ADD COLUMN parent_run_id TEXT;
ALTER TABLE workspace_tasks ADD COLUMN parent_task_id TEXT;
ALTER TABLE workspace_tasks ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workspace_tasks ADD COLUMN cycle_no INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workspace_tasks ADD COLUMN stop_requested_at INTEGER;
ALTER TABLE workspace_tasks ADD COLUMN input_answered_by_json TEXT;
-- Non-NULL iff status = 'failed'.
ALTER TABLE workspace_tasks ADD COLUMN failure_reason TEXT;
UPDATE workspace_tasks SET status = 'failed', failure_reason = 'blocked' WHERE status = 'blocked';
-- A task in flight under the old code has no signal that could dispatch it.
UPDATE workspace_tasks
SET status = 'failed', failure_reason = 'restart',
    error = COALESCE(error, 'task interrupted by app upgrade'),
    completed_at = COALESCE(completed_at, updated_at)
WHERE status IN ('queued', 'running');
UPDATE workspace_tasks
SET failure_reason = CASE WHEN error LIKE '%app restart%' THEN 'restart' ELSE 'error' END
WHERE status = 'failed' AND failure_reason IS NULL;
CREATE INDEX idx_workspace_tasks_parent_task ON workspace_tasks(parent_task_id, created_at);
CREATE INDEX idx_workspace_tasks_parent_session ON workspace_tasks(parent_session_id, created_at);
CREATE INDEX idx_workspace_tasks_parent_run ON workspace_tasks(parent_run_id);
CREATE INDEX idx_workspace_tasks_creator_active ON workspace_tasks(created_by_workspace_agent_id, status);
