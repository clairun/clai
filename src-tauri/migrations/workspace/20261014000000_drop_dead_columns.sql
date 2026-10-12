-- Columns nothing reads. A task the user answered under the old
-- request_user_input flow (writer removed 2026-05) must stay out of the
-- attention count, so its response time becomes the acknowledgement.
UPDATE workspace_tasks
SET attention_acknowledged_at = user_response_at
WHERE user_response_at IS NOT NULL AND attention_acknowledged_at IS NULL;

ALTER TABLE workspace_tasks DROP COLUMN user_response;
ALTER TABLE workspace_tasks DROP COLUMN user_response_at;
-- Only ever {"summary": result_summary}.
ALTER TABLE workspace_tasks DROP COLUMN result_json;
ALTER TABLE assistant_sessions DROP COLUMN kind;
ALTER TABLE assistant_sessions DROP COLUMN title;
ALTER TABLE assistant_runs DROP COLUMN notices_json;
