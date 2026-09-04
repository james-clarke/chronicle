-- 018: the task an interval was first placed under (m30 chunk 2.5). Set on
-- every insert, never touched by merge/reassign, so the replay can rebuild
-- what a since-merged task's own intervals were. NULL for rows that predate
-- this migration.
ALTER TABLE intervals ADD COLUMN origin_task_id INTEGER;
