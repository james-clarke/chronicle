-- All *_ts / ts columns: UTC unix milliseconds.

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL, -- 'focus' | 'title' | 'afk' | 'url'
    app TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    url TEXT,
    pid INTEGER,
    idle INTEGER -- afk events only, 0/1
);
CREATE INDEX idx_events_ts ON events (ts);

CREATE TABLE batches (
    id INTEGER PRIMARY KEY,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending', -- 'pending' | 'running' | 'done' | 'failed'
    attempts INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE spans (
    id INTEGER PRIMARY KEY,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    app TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'focus', -- 'focus' | 'context-switching' | 'afk'
    batch_id INTEGER REFERENCES batches (id)
);
CREATE INDEX idx_spans_start_ts ON spans (start_ts);

CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    batch_id INTEGER NOT NULL REFERENCES batches (id),
    label TEXT NOT NULL,
    project TEXT,
    start_ts INTEGER NOT NULL,
    end_ts INTEGER NOT NULL,
    confidence REAL NOT NULL
);
CREATE INDEX idx_tasks_start_ts ON tasks (start_ts);

CREATE TABLE corrections (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    task_id INTEGER NOT NULL REFERENCES tasks (id),
    old_label TEXT NOT NULL,
    new_label TEXT NOT NULL,
    old_project TEXT,
    new_project TEXT
);

CREATE TABLE chat_messages (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    role TEXT NOT NULL, -- 'user' | 'assistant'
    content TEXT NOT NULL
);

-- External-content FTS. Triggers keep the index in sync; retention pruning
-- deletes through these same triggers.
CREATE VIRTUAL TABLE spans_fts USING fts5 (title, content = 'spans', content_rowid = 'id');
CREATE TRIGGER spans_ai AFTER INSERT ON spans BEGIN
    INSERT INTO spans_fts (rowid, title) VALUES (new.id, new.title);
END;
CREATE TRIGGER spans_ad AFTER DELETE ON spans BEGIN
    INSERT INTO spans_fts (spans_fts, rowid, title) VALUES ('delete', old.id, old.title);
END;
CREATE TRIGGER spans_au AFTER UPDATE OF title ON spans BEGIN
    INSERT INTO spans_fts (spans_fts, rowid, title) VALUES ('delete', old.id, old.title);
    INSERT INTO spans_fts (rowid, title) VALUES (new.id, new.title);
END;

CREATE VIRTUAL TABLE tasks_fts USING fts5 (label, content = 'tasks', content_rowid = 'id');
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
