-- m27 chunk 2: derivation instrumentation. Durations and token counts per
-- derived batch, an insert timestamp on intervals, and job start/finish times.
ALTER TABLE batches ADD COLUMN derived_ts INTEGER;
ALTER TABLE batches ADD COLUMN derive_ms INTEGER;
ALTER TABLE batches ADD COLUMN prompt_tokens INTEGER;
ALTER TABLE batches ADD COLUMN gen_tokens INTEGER;

-- ADD COLUMN cannot default to an expression; the trigger stamps rows that
-- arrive without one. Rows older than this migration stay NULL.
ALTER TABLE intervals ADD COLUMN created_ts INTEGER;
CREATE TRIGGER intervals_created_ts AFTER INSERT ON intervals
WHEN NEW.created_ts IS NULL BEGIN
    UPDATE intervals SET created_ts = CAST(unixepoch('subsec') * 1000 AS INTEGER)
    WHERE id = NEW.id;
END;

ALTER TABLE ai_jobs ADD COLUMN started_ts INTEGER;
ALTER TABLE ai_jobs ADD COLUMN finished_ts INTEGER;
