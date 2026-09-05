-- Account policy is independent of catalog availability: completed cached media
-- can produce listens while the catalog/connection is offline.
CREATE TABLE source_report_account (
    source TEXT PRIMARY KEY CHECK (source != 'local' AND length(source) BETWEEN 1 AND 4096),
    account_key TEXT NOT NULL CHECK (length(account_key) BETWEEN 1 AND 4096),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    accept_new INTEGER NOT NULL CHECK (accept_new IN (0, 1))
);
CREATE TABLE source_report_outbox (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL REFERENCES source_report_account(source) ON DELETE CASCADE,
    account_key TEXT NOT NULL,
    session BLOB NOT NULL CHECK (length(session) = 16),
    kind TEXT NOT NULL DEFAULT 'listen' CHECK (kind = 'listen'),
    location TEXT NOT NULL CHECK (length(location) BETWEEN 1 AND 4096),
    started_at_ms INTEGER NOT NULL CHECK (started_at_ms >= 0),
    created_at_ms INTEGER NOT NULL,
    next_attempt_ms INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    -- 0 pending (including claimed), 1 delivered, 2 failed, 3 cleared.
    state INTEGER NOT NULL DEFAULT 0 CHECK (state BETWEEN 0 AND 3),
    claim_token TEXT,
    claim_until_ms INTEGER,
    last_error TEXT,
    UNIQUE (source, account_key, session, kind),
    CHECK ((claim_token IS NULL) = (claim_until_ms IS NULL))
);
CREATE INDEX source_report_due ON source_report_outbox(state, next_attempt_ms, claim_until_ms, id);
CREATE INDEX source_report_account_state ON source_report_outbox(source, account_key, state);
CREATE INDEX source_report_retention ON source_report_outbox(created_at_ms);
