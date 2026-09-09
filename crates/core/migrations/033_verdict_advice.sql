-- m36 chunk 3: what the pairwise advisor said about a low-margin verdict.
-- NULL = never asked; 'pending' while its job is queued; then 'A' (kept),
-- 'B' (moved to the runner-up), 'new' (minted), 'unsure' (left to
-- confirm), 'invalid' (the answer cited rows outside the segment and was
-- discarded) or 'skipped' (no cloud route; the local model is never asked).
-- Precision of the advisor = outcome over rows with 'A'/'B'/'new'.
ALTER TABLE verdict_log ADD COLUMN advice TEXT;
