-- m36 chunk 2: corrections as memory. A task's label vector (re-embedded
-- when the label changes: `label` is the text the row was made from) and a
-- correction's context vector, both bge-small (384 f32, little-endian,
-- L2-normalised) like migration 022's span vectors. Rows are filled by the
-- daemon tick and `chronicle backfill-embeddings`; a cosine scan over them
-- picks the few-shot corrections a naming or consolidate prompt sees.
CREATE TABLE task_label_embeddings (
    task_id INTEGER PRIMARY KEY,
    label TEXT NOT NULL,
    vec BLOB NOT NULL,
    ts INTEGER NOT NULL
);
CREATE TABLE correction_embeddings (
    correction_id INTEGER PRIMARY KEY,
    vec BLOB NOT NULL
);
