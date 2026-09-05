-- Explicit clearing makes entries unavailable immediately, but an active decoder
-- keeps its open file until release. Startup finishes interrupted removals.
ALTER TABLE source_media_cache ADD COLUMN pending_delete INTEGER NOT NULL DEFAULT 0
    CHECK(pending_delete IN (0, 1));
