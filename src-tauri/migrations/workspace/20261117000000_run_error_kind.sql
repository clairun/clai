-- What a failed run's provider error asks of the user (`FailureKind`, snake_case).
-- NULL for unclassified failures and for runs recorded before this column.
ALTER TABLE assistant_runs ADD COLUMN error_kind TEXT;
