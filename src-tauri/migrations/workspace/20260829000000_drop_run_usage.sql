-- Token usage was recorded per run and read by nothing: no UI surface, no
-- budget logic, and compaction works off its own context estimate. Rather than
-- keep accumulating (and fixing) a number with no consumer, the accounting is
-- removed wholesale. If per-request cost is wanted later it should be recorded
-- as an append-only ledger keyed to the reports as they arrive, not folded into
-- a single per-run blob.
ALTER TABLE assistant_runs DROP COLUMN usage_json;
