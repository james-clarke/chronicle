-- m36 chunk 5: the six numbers per backend, per local day, scored beside
-- self_score once a day from ai_jobs and the verdict log: jobs run, claims
-- made and claims whose evidence resolved (faithfulness before the drop),
-- advisor questions, abstentions ('unsure'), invalid answers, and the
-- changed placements the user later confirmed or undid, plus prompt and
-- cache-read tokens and cost. `backend` is the [backends.<name>] key, or
-- 'local' for the downloaded model. Replay placement and re-run drift are
-- bench numbers and live in meta (`replay_score:<backend>`,
-- `drift:<backend>`, `judge:<backend>`).
CREATE TABLE backend_score (
    day TEXT NOT NULL,
    backend TEXT NOT NULL,
    jobs INTEGER NOT NULL DEFAULT 0,
    claims INTEGER NOT NULL DEFAULT 0,
    claims_ok INTEGER NOT NULL DEFAULT 0,
    advised INTEGER NOT NULL DEFAULT 0,
    unsure INTEGER NOT NULL DEFAULT 0,
    invalid INTEGER NOT NULL DEFAULT 0,
    changed_right INTEGER NOT NULL DEFAULT 0,
    changed_wrong INTEGER NOT NULL DEFAULT 0,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (day, backend)
);
