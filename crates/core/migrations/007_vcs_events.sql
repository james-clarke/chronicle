-- 007: git evidence + task anchors (m15). Vcs events are point markers
-- (checkout, commit) kept out of `events` so they never enter the
-- sessionizer's focus stream. `tasks.external_ref` anchors a task to an
-- external identity (ticket key), set deterministically from branch names.

CREATE TABLE vcs_events (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    -- repo directory name (short), not the full path
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    -- 'checkout' | 'commit'
    kind TEXT NOT NULL,
    -- commit kind only
    commit_id TEXT,
    -- commit subject line; NULL when git(1) was unavailable
    summary TEXT
);
CREATE INDEX idx_vcs_events_ts ON vcs_events (ts);

ALTER TABLE tasks ADD COLUMN external_ref TEXT;
