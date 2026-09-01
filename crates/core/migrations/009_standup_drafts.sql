-- 009: standup drafts. Morning digest of one civil day's journal entries and
-- checkpoints across tasks, drafted by a background ai_job. One row per
-- summarized day; regenerating replaces (mirrors narratives' upsert-by-key).

CREATE TABLE standup_drafts (
    -- Civil day the draft summarizes (ISO YYYY-MM-DD, local at enqueue
    -- time), not the day it was generated on.
    day TEXT PRIMARY KEY,
    ts INTEGER NOT NULL,
    content TEXT NOT NULL
);
