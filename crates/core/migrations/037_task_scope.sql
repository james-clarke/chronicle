-- m44 chunk 1: what a declared task covers besides its ticket. Each row
-- pins one thing to the task: a repo (folder or path), a branch, a
-- document name, a site, or a work-item key. A span carrying one files to
-- the task's project ahead of the project rules, and the task is the
-- sink for that stretch. Rows go with the task.
CREATE TABLE task_scope (
    task_id INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (task_id, kind, value)
);
