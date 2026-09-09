-- m35 chunk 4: the self-score's project invariants. `cross_project` counts
-- the day's non-user placements whose task's project differs from the
-- project most of the focus spans under them are filed in (the number that
-- must read 0); `unfiled_ms` is the day's focus time no project claims (the
-- number to drive down).
ALTER TABLE self_score ADD COLUMN cross_project INTEGER NOT NULL DEFAULT 0;
ALTER TABLE self_score ADD COLUMN unfiled_ms INTEGER NOT NULL DEFAULT 0;
