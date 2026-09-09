-- m36 chunk 4: claims carry evidence. A prose job's output (a task
-- description, a journal entry, a narrative, a standup draft) is stored as
-- text with a footnote marker per claim; this table keeps the claims
-- themselves with the evidence lines each rests on, as JSON, keyed by the
-- output they belong to ('description' → task id, 'journal' → task:batch,
-- 'narrative' → lo:hi, 'standup' → day). ai_jobs gains what the job's
-- faithfulness and cache share were, for the per-backend numbers (chunk 5).
CREATE TABLE claims (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    claims TEXT NOT NULL,
    ts INTEGER NOT NULL,
    PRIMARY KEY (kind, key)
);
ALTER TABLE ai_jobs ADD COLUMN cache_read_tokens INTEGER;
ALTER TABLE ai_jobs ADD COLUMN claims INTEGER;
ALTER TABLE ai_jobs ADD COLUMN claims_ok INTEGER;
