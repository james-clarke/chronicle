-- 015: indexes for hot lookup paths that were doing full scans — chat
-- history by conversation, corrections by task, intervals by time range.
CREATE INDEX IF NOT EXISTS idx_chat_messages_conv ON chat_messages (conversation_id, id);
CREATE INDEX IF NOT EXISTS idx_corrections_task_id ON corrections (task_id);
CREATE INDEX IF NOT EXISTS idx_intervals_range ON intervals (start_ts, end_ts);
