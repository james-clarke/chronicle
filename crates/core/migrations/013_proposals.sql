-- 013: proposed tasks (m24). Unassigned runs that share distinctive title
-- tokens or a repo cluster into one proposal the feed shows as a card; the
-- cluster is keyed by its earliest run's start and rewritten every pre-pass
-- tick while open. `label`/`description` arrive from a suggest_task job;
-- accept declares the task and claims the runs, dismiss parks the cluster.
CREATE TABLE proposals (
    id INTEGER PRIMARY KEY,
    start_ts INTEGER NOT NULL UNIQUE,
    end_ts INTEGER NOT NULL,
    -- focus ms across the cluster's runs
    ms INTEGER NOT NULL,
    -- JSON [[start_ts, end_ts], ...] of the runs, oldest first
    runs TEXT NOT NULL,
    project TEXT,
    label TEXT,
    description TEXT,
    job_id INTEGER REFERENCES ai_jobs (id),
    -- 'open' | 'accepted' | 'dismissed'
    status TEXT NOT NULL DEFAULT 'open',
    task_id INTEGER REFERENCES tasks (id),
    ts INTEGER NOT NULL
);
