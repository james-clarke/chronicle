-- 019: segmenter rows (m30 chunk 3). `source='segment'` intervals carry
-- whether the scorer's margin cleared delta: 0 shows as "to confirm", 1 as
-- placed. NULL for every other source.
ALTER TABLE intervals ADD COLUMN confident INTEGER;
