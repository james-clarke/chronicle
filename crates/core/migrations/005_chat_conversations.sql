-- Chat conversations: every chat message belongs to one conversation so the
-- UI can keep several separate chats. Existing history (if any) is folded
-- into a single backfill conversation; empty databases start with none.
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY,
    created_ts INTEGER NOT NULL
);

ALTER TABLE chat_messages ADD COLUMN conversation_id INTEGER;

INSERT INTO conversations (id, created_ts)
    SELECT 1, COALESCE(MIN(ts), 0) FROM chat_messages HAVING COUNT(*) > 0;

UPDATE chat_messages SET conversation_id = 1;
