-- 006: task descriptions (user-edited or AI-generated) plus a generic queue
-- for local-LLM jobs. Jobs go through the daemon's single-worker scheduler so
-- summarization never competes with derivation for the one resident llama.cpp
-- process. Narratives cache range summaries so reopening a report does not
-- re-run inference.

ALTER TABLE tasks ADD COLUMN description TEXT;

CREATE TABLE ai_jobs (
    id INTEGER PRIMARY KEY,
    -- 'task_description' | 'suggest_task' | 'narrative'
    kind TEXT NOT NULL,
    -- 'pending' | 'running' | 'done' | 'failed'
    status TEXT NOT NULL DEFAULT 'pending',
    -- higher claims first among pending; >= 5 means a user is waiting and the
    -- scheduler skips its idle gate
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_ts INTEGER NOT NULL,
    -- kind-specific JSON (task id, range bounds, ...)
    payload TEXT NOT NULL,
    -- kind-specific JSON/text, set on 'done'
    result TEXT,
    error TEXT
);
CREATE INDEX idx_ai_jobs_status ON ai_jobs (status, priority DESC, created_ts);

CREATE TABLE narratives (
    range_lo INTEGER NOT NULL,
    range_hi INTEGER NOT NULL,
    -- fold over the range's report numbers; a mismatch means the underlying
    -- data changed and the cached text is stale
    data_hash INTEGER NOT NULL,
    text TEXT NOT NULL,
    created_ts INTEGER NOT NULL,
    PRIMARY KEY (range_lo, range_hi)
);
