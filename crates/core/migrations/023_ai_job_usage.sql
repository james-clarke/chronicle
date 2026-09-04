-- m31: which backend ran an AI job and what it cost (NULL for local).
ALTER TABLE ai_jobs ADD COLUMN backend TEXT;
ALTER TABLE ai_jobs ADD COLUMN prompt_tokens INTEGER;
ALTER TABLE ai_jobs ADD COLUMN gen_tokens INTEGER;
ALTER TABLE ai_jobs ADD COLUMN cost_usd REAL;
