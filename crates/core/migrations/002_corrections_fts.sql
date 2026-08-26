-- M5: corrections carry a searchable snapshot of the corrected task's span
-- context (titles + apps) so future batches can FTS-match similar activity.
ALTER TABLE corrections ADD COLUMN ctx TEXT NOT NULL DEFAULT '';

CREATE VIRTUAL TABLE corrections_fts USING fts5 (
    ctx,
    content = 'corrections',
    content_rowid = 'id'
);
CREATE TRIGGER corrections_ai AFTER INSERT ON corrections BEGIN
    INSERT INTO corrections_fts (rowid, ctx) VALUES (new.id, new.ctx);
END;
CREATE TRIGGER corrections_ad AFTER DELETE ON corrections BEGIN
    INSERT INTO corrections_fts (corrections_fts, rowid, ctx) VALUES ('delete', old.id, old.ctx);
END;
