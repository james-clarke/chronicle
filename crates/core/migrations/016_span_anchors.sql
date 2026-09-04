-- 016: span anchors (m30 chunk 1). Typed, tool-neutral evidence on each
-- focus span, written when the tail is sessionized and frozen with the
-- span: work item, change, branch, tool session, calendar event, document,
-- place, people, domain. `activity_events.detail` carries per-kind JSON the
-- collectors used to drop (editor file path and language, AI-session
-- prompts and touched paths, calendar attendees).

CREATE TABLE span_anchors (
    span_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    value TEXT NOT NULL,
    UNIQUE (span_id, kind, value)
);
CREATE INDEX idx_span_anchors_kv ON span_anchors (kind, value);

ALTER TABLE activity_events ADD COLUMN detail TEXT;
