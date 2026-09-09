-- 030 (m35 chunk 1): the declared task the person marked current in its
-- project. The project's sink is this task, else its newest open declared
-- one. At most one per project, kept so by `set_current_task`.
ALTER TABLE tasks ADD COLUMN current INTEGER NOT NULL DEFAULT 0;
