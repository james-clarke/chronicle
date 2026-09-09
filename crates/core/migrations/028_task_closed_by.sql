-- 028: who closed a task. 'user' for a close, merge or fold the person made
-- (sticky: placement never scores it and never reopens it), 'auto' for the
-- idle autoclose (its item, change and event anchors stay scoreable and a
-- placement into it reopens it). Rows closed before this migration stay
-- NULL and count as 'user'.
ALTER TABLE tasks ADD COLUMN closed_by TEXT;
