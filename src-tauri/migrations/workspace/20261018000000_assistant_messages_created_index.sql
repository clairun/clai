-- Newest messages across every session (the Fleet's highlight feed) without
-- sorting the whole table.
CREATE INDEX idx_assistant_messages_created ON assistant_messages(created_at);
