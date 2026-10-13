-- The `retry` and `inter_agent_call` run triggers lost their last producer and
-- were removed; a stored one would fail to deserialize. Retry was a user
-- re-run; an inter-agent call was agent-initiated, closest to a workspace task.
-- Both the JSON-quoted (current) and plain spellings are mapped.
UPDATE assistant_runs SET "trigger" = '"user_message"'
WHERE "trigger" IN ('"retry"', 'retry');
UPDATE assistant_runs SET "trigger" = '"workspace_task"'
WHERE "trigger" IN ('"inter_agent_call"', 'inter_agent_call');
