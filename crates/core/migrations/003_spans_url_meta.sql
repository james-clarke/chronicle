-- M6: per-site spans carry the page URL; meta holds daemon status flags
-- surfaced in the UI (e.g. AW endpoint port conflict).

ALTER TABLE spans ADD COLUMN url TEXT;

CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
