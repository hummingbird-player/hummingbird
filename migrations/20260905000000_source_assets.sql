-- Disposable, account-scoped display assets; never overwrite indexed/user lyrics.
CREATE TABLE source_asset_cache (
    source TEXT NOT NULL REFERENCES library_source(id) ON DELETE CASCADE,
    account_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('lyrics','artwork')),
    locator TEXT NOT NULL CHECK(length(locator) BETWEEN 1 AND 4096),
    content BLOB,
    thumb BLOB,
    checked_at_ms INTEGER NOT NULL,
    accessed_at_ms INTEGER NOT NULL,
    byte_length INTEGER GENERATED ALWAYS AS (COALESCE(length(content),0)+COALESCE(length(thumb),0)) STORED,
    PRIMARY KEY(source,account_key,kind,locator),
    CHECK(source != 'local'),
    CHECK(byte_length <= 8388608)
);
CREATE INDEX source_asset_lru ON source_asset_cache(accessed_at_ms);
