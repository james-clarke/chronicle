-- 029 (m35 chunk 0): the project a focus span is filed into by the rules in
-- config (repo path or place, ticket prefix, domain, title regex, app), so
-- Home, the report and the self-score never re-match. NULL = unfiled, or a
-- span that is not focus. Recomputed on every tail refresh and by
-- `chronicle project rebuild`.
ALTER TABLE spans ADD COLUMN project TEXT;
