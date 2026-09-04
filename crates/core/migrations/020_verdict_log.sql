-- 020: the segmenter's verdicts (m30 chunk 4), one row per reconciled
-- segment interval: the winner, the runner-up, the margin, and what became
-- of it — 'right' (kept, or left alone for a day), 'wrong' (moved, ejected,
-- merged away), NULL while open. `bench --calibrate` reads the margin
-- buckets to set delta from data.
CREATE TABLE verdict_log (
    id INTEGER PRIMARY KEY,
    interval_id INTEGER,
    ts INTEGER NOT NULL,
    task_id INTEGER,
    runner_up INTEGER,
    margin REAL NOT NULL,
    confident INTEGER NOT NULL,
    outcome TEXT
);
CREATE INDEX idx_verdict_log_interval ON verdict_log (interval_id);
CREATE INDEX idx_verdict_log_ts ON verdict_log (ts);
