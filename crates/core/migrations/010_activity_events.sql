-- 010: activity events (m22). `vcs_events` generalizes into one table of
-- point/span markers from any local collector: git checkout/commit (as
-- before), AI coding sessions, PR events, calls. `kind` names the source;
-- `ext_id` is the per-kind external identity (commit hash, session id, PR
-- url, call start) and the dedupe/upsert key; `end_ts` is set for span-like
-- kinds only. Still kept out of `events`: none of this is focus time.

ALTER TABLE vcs_events RENAME TO activity_events;
ALTER TABLE activity_events RENAME COLUMN commit_id TO ext_id;
ALTER TABLE activity_events ADD COLUMN end_ts INTEGER;
CREATE UNIQUE INDEX idx_activity_ext ON activity_events (kind, ext_id, ts) WHERE ext_id IS NOT NULL;
