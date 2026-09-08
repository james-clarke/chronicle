-- 026: the fraction of an interval's wall time that belongs to its task
-- (m32 chunk 3). When two or more AI sessions were writing around one
-- segment, the segmenter emits one row per task over the same range with
-- shares that sum to 1, so reports still add up to captured time. 1.0 for
-- every row placed the ordinary way and for user rows.
ALTER TABLE intervals ADD COLUMN share REAL NOT NULL DEFAULT 1.0;
