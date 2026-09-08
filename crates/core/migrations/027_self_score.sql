-- 027: the derivation's daily self-score (m32 chunk 6). One row per local
-- day, recomputed by the daemon once a day over the last week, so a
-- correction that lands later still counts against the day it judges.
-- Counts and milliseconds only; every rate is rendered, never stored.
CREATE TABLE self_score (
    day TEXT PRIMARY KEY,            -- local civil date, YYYY-MM-DD
    computed_ts INTEGER NOT NULL,
    active_ms INTEGER NOT NULL,      -- non-AFK span time
    uncaptured_ms INTEGER NOT NULL,  -- ledger gaps (daemon_runs)
    underived_ms INTEGER NOT NULL,   -- active time no done batch covers
    placed_ms INTEGER NOT NULL,      -- interval time times share
    minted INTEGER NOT NULL,         -- derived tasks born that day
    merged INTEGER NOT NULL,         -- of those, folded into another task within 24 h
    placements INTEGER NOT NULL,     -- non-user intervals written that day
    ejects INTEGER NOT NULL,         -- eject corrections that day
    renames INTEGER NOT NULL,        -- rename corrections that day
    verdicts INTEGER NOT NULL,       -- closed verdicts placed that day
    wrong INTEGER NOT NULL,
    confident INTEGER NOT NULL,
    confident_wrong INTEGER NOT NULL
);
-- A merge remembers its source: the source row goes with the merge when
-- nothing else references it, and "minted, then merged within a day" needs
-- its id (to count the birth once) and its birth time.
ALTER TABLE corrections ADD COLUMN src_task_id INTEGER;
ALTER TABLE corrections ADD COLUMN src_created_ts INTEGER;
