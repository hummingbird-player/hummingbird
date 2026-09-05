-- no-transaction
-- Rebuild without invoking child cascades or album/artist cleanup. IDs and every
-- existing column are copied verbatim; the connection is restored to FK enforcement.
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE library_source (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) > 0),
    kind TEXT NOT NULL,
    sync_generation INTEGER NOT NULL DEFAULT 0,
    completed_generation INTEGER NOT NULL DEFAULT 0,
    last_success_at DATETIME,
    sync_cursor TEXT,
    CHECK (id != 'local' OR kind = 'local')
);
INSERT INTO library_source (id, kind) VALUES ('local', 'local');

ALTER TABLE album ADD COLUMN source TEXT NOT NULL DEFAULT 'local' REFERENCES library_source(id);
DROP INDEX album_title_override_mbid;
CREATE UNIQUE INDEX album_title_override_mbid
    ON album (title, artist_display_override, mbid) WHERE source = 'local';
CREATE INDEX idx_album_source ON album (source, id);

DROP TRIGGER delete_album_path_trigger;
DROP TRIGGER delete_album_trigger;
DROP TRIGGER update_track_album_cleanup;
DROP TRIGGER delete_artist_trigger;
DROP TRIGGER delete_track_artist_trigger;

CREATE TABLE track_new (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    title_sortable TEXT NOT NULL,
    album_id INTEGER,
    track_number INTEGER,
    disc_number INTEGER,
    duration INTEGER NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    tags TEXT,
    location TEXT NOT NULL, artist_names TEXT, folder TEXT, rg_track_gain REAL, rg_track_peak REAL, rg_album_gain REAL, rg_album_peak REAL, disc_subtitle TEXT, artists TEXT, artist_sort TEXT, album_artist_keys TEXT, artwork_id INTEGER REFERENCES artwork(id), art_hash INTEGER, release_date DATE, date_precision INTEGER, track_section INTEGER, number_display_mode_hint INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'local' REFERENCES library_source(id),
    present INTEGER NOT NULL DEFAULT 1 CHECK (present IN (0, 1)),
    sync_generation INTEGER NOT NULL DEFAULT 0,
    UNIQUE (source, location),
    CHECK (source = 'local' OR folder IS NULL),
    FOREIGN KEY (album_id) REFERENCES album (id)
);

INSERT INTO track_new (id, title, title_sortable, album_id, track_number, disc_number, duration, created_at, tags, location, artist_names, folder, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, disc_subtitle, artists, artist_sort, album_artist_keys, artwork_id, art_hash, release_date, date_precision, track_section, number_display_mode_hint) SELECT id, title, title_sortable, album_id, track_number, disc_number, duration, created_at, tags, location, artist_names, folder, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, disc_subtitle, artists, artist_sort, album_artist_keys, artwork_id, art_hash, release_date, date_precision, track_section, number_display_mode_hint FROM track;
DROP TABLE track;
ALTER TABLE track_new RENAME TO track;

CREATE INDEX idx_track_album_id ON track (album_id, id);
CREATE INDEX idx_track_artwork_id ON track(artwork_id) WHERE artwork_id IS NOT NULL;
CREATE INDEX idx_track_source_presence ON track (source, present, id);
CREATE TRIGGER delete_album_path_trigger AFTER DELETE ON track BEGIN
DELETE FROM album_path
WHERE
    album_path.path = OLD.folder
    AND album_path.disc_num = IFNULL (OLD.disc_number, -1)
    AND album_path.album_id = OLD.album_id
    AND NOT EXISTS (
        SELECT
            1
        FROM
            track
        WHERE
            track.folder = OLD.folder
            AND IFNULL(track.disc_number, -1) = IFNULL(OLD.disc_number, -1)
            AND track.album_id = OLD.album_id
    );

END;

CREATE TRIGGER delete_album_trigger AFTER DELETE ON track
BEGIN
    DELETE FROM album
    WHERE album.id = OLD.album_id
    AND NOT EXISTS (
        SELECT 1
        FROM track
        WHERE track.album_id = OLD.album_id
    );
END;

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
    WHERE album.id = OLD.album_id
    AND NOT EXISTS (
        SELECT 1
        FROM track
        WHERE track.album_id = OLD.album_id
    );
END;

CREATE TRIGGER delete_artist_trigger AFTER DELETE ON album_artist
BEGIN
    DELETE FROM artist
    WHERE artist.id = OLD.artist_id
    AND NOT EXISTS (
        SELECT 1
        FROM album_artist
        WHERE album_artist.artist_id = artist.id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM track_artist
        WHERE track_artist.artist_id = artist.id
    );
END;

CREATE TRIGGER delete_track_artist_trigger AFTER DELETE ON track_artist
BEGIN
    DELETE FROM artist
    WHERE artist.id = OLD.artist_id
    AND NOT EXISTS (
        SELECT 1
        FROM album_artist
        WHERE album_artist.artist_id = artist.id
    )
    AND NOT EXISTS (
        SELECT 1
        FROM track_artist
        WHERE track_artist.artist_id = artist.id
    );
END;

-- Enforce ownership at the database boundary as well as in the host writer.
CREATE TRIGGER track_album_source_insert BEFORE INSERT ON track
WHEN NEW.album_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM album WHERE id = NEW.album_id AND source = NEW.source
) BEGIN SELECT RAISE(ABORT, 'track and album sources differ'); END;
CREATE TRIGGER track_album_source_update BEFORE UPDATE OF album_id, source ON track
WHEN NEW.album_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM album WHERE id = NEW.album_id AND source = NEW.source
) BEGIN SELECT RAISE(ABORT, 'track and album sources differ'); END;
CREATE TRIGGER album_source_immutable BEFORE UPDATE OF source ON album
WHEN NEW.source != OLD.source
BEGIN SELECT RAISE(ABORT, 'album source is immutable'); END;
CREATE TRIGGER track_source_immutable BEFORE UPDATE OF source ON track
WHEN NEW.source != OLD.source
BEGIN SELECT RAISE(ABORT, 'track source is immutable'); END;
CREATE TRIGGER album_path_local_insert BEFORE INSERT ON album_path
WHEN NOT EXISTS (SELECT 1 FROM album WHERE id = NEW.album_id AND source = 'local')
BEGIN SELECT RAISE(ABORT, 'remote albums cannot claim folders'); END;
CREATE TRIGGER album_path_local_update BEFORE UPDATE OF album_id ON album_path
WHEN NOT EXISTS (SELECT 1 FROM album WHERE id = NEW.album_id AND source = 'local')
BEGIN SELECT RAISE(ABORT, 'remote albums cannot claim folders'); END;
CREATE TRIGGER preserve_local_source BEFORE DELETE ON library_source
WHEN OLD.id = 'local'
BEGIN SELECT RAISE(ABORT, 'local source is reserved'); END;

CREATE TRIGGER preserve_local_source_update BEFORE UPDATE OF id, kind ON library_source
WHEN OLD.id = 'local' AND (NEW.id != 'local' OR NEW.kind != 'local')
BEGIN SELECT RAISE(ABORT, 'local source is reserved'); END;

CREATE UNIQUE INDEX album_source_id ON album (source, id);
CREATE TABLE remote_album (
    source TEXT NOT NULL REFERENCES library_source(id),
    remote_id TEXT NOT NULL,
    album_id INTEGER NOT NULL UNIQUE,
    PRIMARY KEY (source, remote_id),
    FOREIGN KEY (source, album_id) REFERENCES album(source, id) ON DELETE CASCADE
);
CREATE TABLE remote_artist (
    source TEXT NOT NULL REFERENCES library_source(id),
    remote_id TEXT NOT NULL,
    artist_id INTEGER NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
    PRIMARY KEY (source, remote_id)
);
CREATE TABLE remote_playlist (
    source TEXT NOT NULL REFERENCES library_source(id),
    remote_id TEXT NOT NULL,
    playlist_id INTEGER NOT NULL UNIQUE REFERENCES playlist(id) ON DELETE CASCADE,
    revision TEXT,
    baseline TEXT,
    writable INTEGER NOT NULL DEFAULT 0 CHECK (writable IN (0, 1)),
    PRIMARY KEY (source, remote_id)
);

-- Abort rather than leave a partly valid migration. Unlike a bare PRAGMA this
-- assertion fails the transaction if any dependent reference was damaged.
CREATE TEMP TABLE source_migration_fk_check (violations INTEGER CHECK (violations = 0));
INSERT INTO source_migration_fk_check SELECT COUNT(*) FROM pragma_foreign_key_check;
DROP TABLE source_migration_fk_check;
COMMIT;
PRAGMA foreign_keys = ON;
