-- Session ancestry links were never produced: the only writer was
-- `assistant_create_session` with a parent id the frontend never sent, and
-- compaction rotates the CLI session id in place rather than creating a child
-- session. Message paging now reads a single session, so the table goes.
DROP TABLE IF EXISTS assistant_session_links;
