-- no-transaction
-- rebuild track to replace UNIQUE(location)
-- turn off foreign keys before BEGIN so dropping the old table doesn't delete related rows
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE library_source (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL
);
INSERT INTO library_source (id, kind) VALUES ('local', 'local');

ALTER TABLE album ADD COLUMN source TEXT NOT NULL DEFAULT 'local'
    REFERENCES library_source(id);
DROP INDEX album_title_override_mbid;
CREATE UNIQUE INDEX album_title_override_mbid
    ON album (title, artist_display_override, mbid) WHERE source = 'local';
CREATE UNIQUE INDEX album_id_source ON album(id, source);

CREATE TABLE source_album (
    source TEXT NOT NULL REFERENCES library_source(id),
    location TEXT NOT NULL,
    album_id INTEGER NOT NULL UNIQUE,
    PRIMARY KEY (source, location),
    FOREIGN KEY (album_id, source) REFERENCES album(id, source) ON DELETE CASCADE
);

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
    location TEXT NOT NULL,
    artist_names TEXT,
    folder TEXT,
    rg_track_gain REAL,
    rg_track_peak REAL,
    rg_album_gain REAL,
    rg_album_peak REAL,
    disc_subtitle TEXT,
    artists TEXT,
    artist_sort TEXT,
    album_artist_keys TEXT,
    artwork_id INTEGER REFERENCES artwork(id),
    art_hash INTEGER,
    release_date DATE,
    date_precision INTEGER,
    track_section INTEGER,
    number_display_mode_hint INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'local' REFERENCES library_source(id),
    UNIQUE (source, location),
    FOREIGN KEY (album_id, source) REFERENCES album(id, source),
    CHECK (source = 'local' OR folder IS NULL)
);
INSERT INTO track_new
SELECT track.*, 'local' FROM track;

DROP TRIGGER delete_album_path_trigger;
DROP TRIGGER delete_album_trigger;
DROP TRIGGER update_track_album_cleanup;
DROP TABLE track;
ALTER TABLE track_new RENAME TO track;

CREATE INDEX idx_track_album_id ON track (album_id, id);
CREATE INDEX idx_track_artwork_id ON track(artwork_id) WHERE artwork_id IS NOT NULL;

CREATE TRIGGER delete_album_path_trigger AFTER DELETE ON track
WHEN OLD.source = 'local'
BEGIN
    DELETE FROM album_path
    WHERE path = OLD.folder
      AND disc_num = IFNULL(OLD.disc_number, -1)
      AND album_id = OLD.album_id
      AND NOT EXISTS (
          SELECT 1 FROM track
          WHERE source = 'local' AND folder = OLD.folder
            AND IFNULL(disc_number, -1) = IFNULL(OLD.disc_number, -1)
            AND album_id = OLD.album_id
      );
END;

CREATE TRIGGER delete_album_trigger AFTER DELETE ON track
BEGIN
    DELETE FROM album WHERE id = OLD.album_id
      AND NOT EXISTS (SELECT 1 FROM track WHERE album_id = OLD.album_id);
END;

CREATE TRIGGER update_track_album_cleanup AFTER UPDATE OF album_id ON track
WHEN OLD.album_id IS NOT NULL AND (NEW.album_id IS NULL OR OLD.album_id != NEW.album_id)
BEGIN
    DELETE FROM album_path
    WHERE OLD.source = 'local'
      AND path = OLD.folder
      AND disc_num = IFNULL(OLD.disc_number, -1)
      AND album_id = OLD.album_id
      AND NOT EXISTS (
          SELECT 1 FROM track
          WHERE source = 'local' AND folder = OLD.folder
            AND IFNULL(disc_number, -1) = IFNULL(OLD.disc_number, -1)
            AND album_id = OLD.album_id
      );
    DELETE FROM album WHERE id = OLD.album_id
      AND NOT EXISTS (SELECT 1 FROM track WHERE album_id = OLD.album_id);
END;

COMMIT;
PRAGMA foreign_keys = ON;
