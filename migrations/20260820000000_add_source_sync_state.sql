ALTER TABLE library_source ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE library_source ADD COLUMN completed_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE library_source ADD COLUMN last_sync_completed_at DATETIME;

ALTER TABLE source_album ADD COLUMN last_seen_generation INTEGER NOT NULL DEFAULT 0;

ALTER TABLE track ADD COLUMN source_generation INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_source_album_generation
    ON source_album(source, last_seen_generation);
CREATE INDEX idx_track_source_generation
    ON track(source, source_generation);
