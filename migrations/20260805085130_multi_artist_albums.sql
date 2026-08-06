-- no-transaction

PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE album_artist (
    album_id INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    FOREIGN KEY (album_id) REFERENCES album (id),
    FOREIGN KEY (artist_id) REFERENCES artist (id),
    PRIMARY KEY (album_id, artist_id)
);

INSERT INTO album_artist (album_id, artist_id)
SELECT id, artist_id
FROM album
WHERE artist_id IS NOT NULL
  AND artist_id IN (SELECT id FROM artist);

-- artist-less albums get a placeholder artist so they stay browsable and sortable, this also
-- covers dangling artist_id values from databases scanned without foreign key enforcement
INSERT INTO artist (name, name_sortable)
SELECT 'Unknown Artist', 'Unknown Artist'
WHERE EXISTS (
    SELECT 1
    FROM album
    WHERE artist_id IS NULL
       OR artist_id NOT IN (SELECT id FROM artist)
)
  AND NOT EXISTS (SELECT 1 FROM artist WHERE name = 'Unknown Artist');

INSERT INTO album_artist (album_id, artist_id)
SELECT album.id, artist.id
FROM album
JOIN artist ON artist.name = 'Unknown Artist'
WHERE album.artist_id IS NULL
   OR album.artist_id NOT IN (SELECT id FROM artist);

-- raw Artists tag plus the sort tag it was seen with, used to derive album_artist rows on scan
ALTER TABLE track ADD COLUMN artists TEXT;
ALTER TABLE track ADD COLUMN artist_sort TEXT;

DROP TRIGGER delete_artist_trigger;
DROP TRIGGER update_track_album_cleanup;
-- references the album table, must not dangle while it is rebuilt
DROP TRIGGER delete_album_trigger;

CREATE TABLE album_new (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    title_sortable TEXT NOT NULL,
    artist_display_override TEXT NOT NULL DEFAULT '',
    release_date DATE,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    image BLOB,
    thumb BLOB,
    tags TEXT,
    label TEXT,
    catalog_number TEXT,
    isrc TEXT,
    mbid TEXT DEFAULT "none" NOT NULL,
    vinyl_numbering INTEGER DEFAULT 0 NOT NULL,
    date_precision INTEGER
);

INSERT INTO album_new (
    id,
    title,
    title_sortable,
    artist_display_override,
    release_date,
    created_at,
    image,
    thumb,
    tags,
    label,
    catalog_number,
    isrc,
    mbid,
    vinyl_numbering,
    date_precision
)
SELECT
    album.id,
    album.title,
    album.title_sortable,
    COALESCE(artist.name, ''),
    album.release_date,
    album.created_at,
    album.image,
    album.thumb,
    album.tags,
    album.label,
    album.catalog_number,
    album.isrc,
    album.mbid,
    album.vinyl_numbering,
    album.date_precision
FROM album
LEFT JOIN artist ON artist.id = album.artist_id;

DROP TABLE album;

ALTER TABLE album_new RENAME TO album;

-- legacy databases can hold multiple artist-less albums sharing one title and mbid, the old
-- unique index treated NULL artist_id as distinct
CREATE TEMP TABLE album_kept AS
SELECT MIN(id) AS id, title, artist_display_override, mbid
FROM album
GROUP BY title, artist_display_override, mbid;

CREATE INDEX album_kept_group ON album_kept (title, artist_display_override, mbid);

-- repoint tracks at collapsed albums, the survivor id equals the album's own id elsewhere
UPDATE track
SET album_id = (
    SELECT kept.id
    FROM album_kept kept
    JOIN album victim ON victim.id = track.album_id
    WHERE kept.title = victim.title
      AND kept.artist_display_override = victim.artist_display_override
      AND kept.mbid = victim.mbid
)
WHERE track.album_id IS NOT NULL;

DELETE FROM album
WHERE id NOT IN (SELECT id FROM album_kept);

-- the delete triggers are recreated below, clean up the removed albums explicitly
DELETE FROM album_path
WHERE album_path.album_id NOT IN (SELECT id FROM album);

DELETE FROM album_artist
WHERE album_artist.album_id NOT IN (SELECT id FROM album);

DROP TABLE album_kept;

CREATE UNIQUE INDEX IF NOT EXISTS album_title_override_mbid
    ON album (title, artist_display_override, mbid);
CREATE INDEX IF NOT EXISTS album_release_date_idx ON album (release_date);
CREATE INDEX IF NOT EXISTS idx_album_artist_artist ON album_artist (artist_id, album_id);

CREATE TRIGGER delete_album_paths AFTER DELETE ON album BEGIN
DELETE FROM album_path
WHERE
    album_path.album_id = OLD.id;

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

CREATE TRIGGER delete_album_artists AFTER DELETE ON album BEGIN
DELETE FROM album_artist
WHERE
    album_artist.album_id = OLD.id;

END;

CREATE TRIGGER delete_artist_trigger AFTER DELETE ON album_artist
BEGIN
    DELETE FROM artist
    WHERE artist.id = OLD.artist_id
    AND NOT EXISTS (
        SELECT 1
        FROM album_artist
        WHERE album_artist.artist_id = artist.id
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

COMMIT;

PRAGMA foreign_keys = ON;
