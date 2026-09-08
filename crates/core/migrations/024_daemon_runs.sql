-- m32 chunk 0: capture ledger. One row per stretch the daemon was capturing;
-- end_ts is NULL while it runs. reason: 'shutdown' | 'provider exit' |
-- 'lock' | 'sleep' | 'crash'.
CREATE TABLE daemon_runs (
    id INTEGER PRIMARY KEY,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER,
    reason TEXT
);
CREATE INDEX idx_daemon_runs_start_ts ON daemon_runs (start_ts);
