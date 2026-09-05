-- Host-owned files retain their source identity. Partial reservations count
-- against the budget but are never candidates for offline playback.
CREATE TABLE source_media_cache (
    token TEXT PRIMARY KEY NOT NULL CHECK(length(token) = 32),
    source TEXT NOT NULL CHECK(source != 'local'),
    location TEXT NOT NULL,
    profile TEXT NOT NULL,
    revision TEXT NOT NULL,
    complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0, 1)),
    size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
    format TEXT,
    checksum TEXT,
    offline INTEGER NOT NULL DEFAULT 0 CHECK(offline IN (0, 1)),
    validated_at_ms INTEGER NOT NULL,
    accessed_at_ms INTEGER NOT NULL,
    FOREIGN KEY(source, location) REFERENCES track(source, location) ON DELETE CASCADE,
    CHECK(complete = 0 OR checksum IS NOT NULL)
);
CREATE INDEX source_media_cache_lookup
    ON source_media_cache(source, location, profile, complete, validated_at_ms DESC);
CREATE INDEX source_media_cache_eviction
    ON source_media_cache(source, complete, offline, accessed_at_ms);
