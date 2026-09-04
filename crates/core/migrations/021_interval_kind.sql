-- 021: what kind of work a segment was (m30 chunk 5): author, agent,
-- review, communicate, meet, plan, read, admin, break — from the app
-- families and anchors of its spans. Written by the segmenter; NULL for
-- rows from the model path and for user rows.
ALTER TABLE intervals ADD COLUMN kind TEXT;
