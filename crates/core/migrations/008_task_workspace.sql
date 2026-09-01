-- 008: task workspace (m16). Context fetched via MCP, append-only journal
-- derived per batch, latest-only checkpoint, task-scoped conversations.
-- Workspace rows cascade with their task/batch so the existing delete sites
-- (orphan-task guard, retention prune) need no changes; a conversation
-- outlives its task as a general (unscoped) thread.

-- One context bundle per task (one external_ref per task); re-fetch replaces.
CREATE TABLE task_context (
    task_id INTEGER PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
    -- provenance, e.g. 'mcp'
    source TEXT NOT NULL,
    fetched_ts INTEGER NOT NULL,
    content TEXT NOT NULL
);

CREATE TABLE journal_entries (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    batch_id INTEGER NOT NULL REFERENCES batches (id) ON DELETE CASCADE,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    entry TEXT NOT NULL,
    -- json array of interval ids backing the entry
    evidence TEXT
);
CREATE INDEX idx_journal_task ON journal_entries (task_id, start_ts);
-- One entry per batch per task; a re-derived batch replaces its entry.
CREATE UNIQUE INDEX idx_journal_task_batch ON journal_entries (task_id, batch_id);

-- Latest "where I am / what's next" only; regenerated, overwrites.
CREATE TABLE checkpoints (
    task_id INTEGER PRIMARY KEY REFERENCES tasks (id) ON DELETE CASCADE,
    ts INTEGER NOT NULL,
    state TEXT NOT NULL,
    next_steps TEXT NOT NULL
);

-- Task-scoped chat threads: at most one conversation per task; general
-- conversations keep task_id NULL, unconstrained.
ALTER TABLE conversations ADD COLUMN task_id INTEGER REFERENCES tasks (id) ON DELETE SET NULL;
CREATE UNIQUE INDEX idx_conversations_task_id
    ON conversations (task_id) WHERE task_id IS NOT NULL;
