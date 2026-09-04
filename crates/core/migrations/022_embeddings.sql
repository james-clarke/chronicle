-- 022: the soft tier's vectors (m30 chunk 6). A span's title embedding
-- (L2-normalised f32 little-endian blob) and, per task, the minute-weighted
-- centroid of the spans under its intervals. Rows exist only when
-- `embed_model` is set; the scorer adds a bounded cosine bonus when both
-- sides carry a vector and otherwise behaves as before.
CREATE TABLE span_embeddings (
    span_id INTEGER PRIMARY KEY,
    vec BLOB NOT NULL
);
CREATE TABLE task_embeddings (
    task_id INTEGER PRIMARY KEY,
    vec BLOB NOT NULL
);
