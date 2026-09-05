ALTER TABLE library_source ADD COLUMN configuration_token TEXT NOT NULL DEFAULT '';
ALTER TABLE library_source ADD COLUMN configuration_key TEXT NOT NULL DEFAULT '';
ALTER TABLE library_source ADD COLUMN sync_scope TEXT;
ALTER TABLE library_source ADD COLUMN completed_scope TEXT;

-- Remote metadata is operational state, not a second song identity. The only
-- source/song key remains track(source, location).
CREATE TABLE source_track (
    track_id INTEGER PRIMARY KEY REFERENCES track(id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
    artwork_locator TEXT,
    content_revision TEXT,
    original_format TEXT,
    original_bitrate_kbps INTEGER,
    musicbrainz_id TEXT,
    starred_baseline INTEGER CHECK (starred_baseline IN (0, 1)),
    rating_baseline INTEGER CHECK (rating_baseline BETWEEN 0 AND 5)
);
CREATE INDEX source_track_scope ON source_track(scope, track_id);
ALTER TABLE remote_album ADD COLUMN artwork_locator TEXT;
ALTER TABLE remote_album ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN rating INTEGER CHECK (rating BETWEEN 0 AND 5);

CREATE TRIGGER source_track_remote_only BEFORE INSERT ON source_track
WHEN NOT EXISTS (SELECT 1 FROM track WHERE id = NEW.track_id AND source != 'local')
BEGIN SELECT RAISE(ABORT, 'source metadata requires a remote track'); END;

-- Remote albums retain stable mappings across retags and empty snapshots.
DROP TRIGGER delete_album_trigger;
CREATE TRIGGER delete_album_trigger AFTER DELETE ON track
BEGIN
    DELETE FROM album
    WHERE album.id = OLD.album_id AND album.source = 'local'
    AND NOT EXISTS (
        SELECT 1
        FROM track
        WHERE track.album_id = OLD.album_id
    );
END;


-- Remote albums retain stable mappings across retags and empty snapshots.
DROP TRIGGER update_track_album_cleanup;
CREATE TRIGGER update_track_album_cleanup AFTER UPDATE OF album_id ON track
WHEN OLD.album_id IS NOT NULL AND (NEW.album_id IS NULL OR OLD.album_id != NEW.album_id)
BEGIN
    DELETE FROM album_path
    WHERE
        album_path.path = OLD.folder
        AND album_path.disc_num = IFNULL(OLD.disc_number, -1)
        AND album_path.album_id = OLD.album_id
        AND NOT EXISTS (
            SELECT 1
            FROM track
            WHERE track.folder = OLD.folder
              AND IFNULL(track.disc_number, -1) = IFNULL(OLD.disc_number, -1)
              AND track.album_id = OLD.album_id
        );

    DELETE FROM album
    WHERE album.id = OLD.album_id AND album.source = 'local'
    AND NOT EXISTS (
        SELECT 1
        FROM track
        WHERE track.album_id = OLD.album_id
    );
END;
