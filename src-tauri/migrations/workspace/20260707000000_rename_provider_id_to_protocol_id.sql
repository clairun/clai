-- Rename the run/compaction metadata column provider_id -> protocol_id so the
-- column name matches what it stores (the wire protocol adapter key), after the
-- ProviderConnection.provider_id -> protocol_id split. RENAME COLUMN preserves
-- all existing run/compaction history. (The brand id lives on the connection
-- config, not in these tables.)
ALTER TABLE assistant_runs RENAME COLUMN provider_id TO protocol_id;
ALTER TABLE assistant_compactions RENAME COLUMN provider_id TO protocol_id;
