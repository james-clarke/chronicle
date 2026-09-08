-- m32 chunk 1: idle folded into a span as quiet time, and per-minute input
-- counts (never what was typed).
ALTER TABLE spans ADD COLUMN quiet_ms INTEGER NOT NULL DEFAULT 0;

CREATE TABLE presence (
    minute_ts INTEGER PRIMARY KEY, -- ms, minute-aligned
    keys INTEGER NOT NULL DEFAULT 0,
    buttons INTEGER NOT NULL DEFAULT 0,
    motion INTEGER NOT NULL DEFAULT 0,
    scroll INTEGER NOT NULL DEFAULT 0
);
