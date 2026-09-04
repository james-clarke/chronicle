-- 017: task evidence (m30 chunk 2). One row per task per evidence key
-- (anchor kind/value, or a title term) summing decayed minutes across
-- sources (interval, correction, declared). Rebuilt wholesale by
-- `rebuild_task_evidence` from `profile::build_evidence`, so rows cascade
-- with their task the same as the other task-scoped tables (008).

CREATE TABLE task_evidence (
    task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    value TEXT NOT NULL,
    source TEXT NOT NULL,
    minutes REAL NOT NULL,
    first_ts INTEGER NOT NULL,
    last_ts INTEGER NOT NULL,
    PRIMARY KEY (task_id, kind, value, source)
);
CREATE INDEX idx_task_evidence_kv ON task_evidence (kind, value);
