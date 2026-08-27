-- Derive v3: tasks become identities ("what"), intervals carry the time
-- ("when"). A task spans batches; each derived interval links to one task.
-- Existing task rows migrate 1:1 (identity + one interval), no retro dedup.
-- NOTE: requires foreign_keys OFF during migration (table rebuild with
-- children); open() re-enables it afterwards.

CREATE TABLE intervals (
    id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    batch_id INTEGER NOT NULL REFERENCES batches (id),
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    confidence REAL NOT NULL
);
CREATE INDEX idx_intervals_start_ts ON intervals (start_ts);
CREATE INDEX idx_intervals_task_id ON intervals (task_id);

INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
SELECT id, batch_id, start_ts, end_ts, confidence FROM tasks;

CREATE TABLE tasks_v3 (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL,
    project TEXT,
    status TEXT NOT NULL DEFAULT 'open', -- 'open' | 'closed'
    source TEXT NOT NULL DEFAULT 'derived', -- 'user' | 'derived'
    created_ts INTEGER NOT NULL,
    closed_ts INTEGER
);
INSERT INTO tasks_v3 (id, label, project, status, source, created_ts)
SELECT id, label, project, 'open', 'derived', start_ts FROM tasks;

-- Dropping tasks also drops its tasks_ai/ad/au triggers; corrections keep
-- their task_id values, which stay valid because ids are copied verbatim.
DROP TABLE tasks;
ALTER TABLE tasks_v3 RENAME TO tasks;
CREATE INDEX idx_tasks_created_ts ON tasks (created_ts);

CREATE TRIGGER tasks_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts (rowid, label) VALUES (new.id, new.label);
END;
CREATE TRIGGER tasks_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, label) VALUES ('delete', old.id, old.label);
END;
CREATE TRIGGER tasks_au AFTER UPDATE OF label ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, label) VALUES ('delete', old.id, old.label);
    INSERT INTO tasks_fts (rowid, label) VALUES (new.id, new.label);
END;
INSERT INTO tasks_fts (tasks_fts) VALUES ('rebuild');

-- Corrections gain a kind; 'reassign' moves one interval to another task.
ALTER TABLE corrections ADD COLUMN kind TEXT NOT NULL DEFAULT 'rename'; -- 'rename' | 'reassign'
ALTER TABLE corrections ADD COLUMN interval_id INTEGER REFERENCES intervals (id);
