-- m44 chunk 2: derived work is a proposal until confirmed. `source` says
-- where a proposal came from: `runs` (unassigned runs the pre-pass could
-- not place, clustered by title words or repo) or `segment` (a cluster
-- the segmenter would have minted a task for; its time sits on the
-- project's other work until confirmed). Confirming a segment proposal
-- creates the task and moves the cluster's time onto it. A cluster is
-- keyed by its earliest stretch within its source, so the two sources
-- never block each other's row; the table is rebuilt for that key.
CREATE TABLE proposals_new (
    id INTEGER PRIMARY KEY,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    ms INTEGER NOT NULL,
    runs TEXT NOT NULL,
    project TEXT,
    label TEXT,
    description TEXT,
    job_id INTEGER REFERENCES ai_jobs (id),
    status TEXT NOT NULL DEFAULT 'open',
    task_id INTEGER REFERENCES tasks (id),
    ts INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT 'runs',
    UNIQUE (source, start_ts)
);
INSERT INTO proposals_new (id, start_ts, end_ts, ms, runs, project, label, description, job_id, status, task_id, ts)
    SELECT id, start_ts, end_ts, ms, runs, project, label, description, job_id, status, task_id, ts FROM proposals;
DROP TABLE proposals;
ALTER TABLE proposals_new RENAME TO proposals;
