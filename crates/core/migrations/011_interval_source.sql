-- 011: intervals carry who placed them (m24). 'derived' = model batch,
-- 'user' = assign/reassign by hand, 'prepass' = deterministic pre-pass
-- (provisional, replaced by the next derive). Existing user assigns (m23)
-- are recognised by their 'assign' correction + confidence 1.0.
ALTER TABLE intervals ADD COLUMN source TEXT NOT NULL DEFAULT 'derived';
UPDATE intervals SET source='user'
 WHERE id IN (SELECT interval_id FROM corrections WHERE kind='assign' AND interval_id IS NOT NULL)
    OR (confidence >= 1.0 AND task_id IN (SELECT task_id FROM corrections WHERE kind='assign'));
