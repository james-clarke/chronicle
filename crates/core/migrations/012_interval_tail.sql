-- 012: intervals over the live tail (m24). The pre-pass places provisional
-- intervals, and the user keeps/moves blocks, before the sessionizer has
-- closed a batch around them, so batch_id becomes nullable (attached when the
-- batch closes, see storage::replace_tail). `reason` is the one-line why a
-- pre-pass placed a block ("branch ACME-7", "repo chronicle", "past
-- correction"); NULL for derived and user intervals. Table rebuild (SQLite
-- cannot drop NOT NULL in place); ids copied verbatim so corrections keep
-- their interval refs. Requires foreign_keys OFF (open() handles it).
CREATE TABLE intervals_v2 (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    batch_id INTEGER REFERENCES batches (id),
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    confidence REAL NOT NULL,
    source TEXT NOT NULL DEFAULT 'derived', -- 'derived' | 'user' | 'prepass'
    reason TEXT
);
INSERT INTO intervals_v2 (id, task_id, batch_id, start_ts, end_ts, confidence, source)
SELECT id, task_id, batch_id, start_ts, end_ts, confidence, source FROM intervals;
DROP TABLE intervals;
ALTER TABLE intervals_v2 RENAME TO intervals;
CREATE INDEX idx_intervals_start_ts ON intervals (start_ts);
CREATE INDEX idx_intervals_task_id ON intervals (task_id);
