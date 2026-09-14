-- m44 chunk 2: derived work is a proposal until confirmed. `source` says
-- where a proposal came from: `runs` (unassigned runs the pre-pass could
-- not place, clustered by title words or repo) or `segment` (a cluster
-- the segmenter would have minted a task for; its time sits on the
-- project's other work until confirmed). Confirming a segment proposal
-- creates the task and moves the cluster's time onto it.
ALTER TABLE proposals ADD COLUMN source TEXT NOT NULL DEFAULT 'runs';
