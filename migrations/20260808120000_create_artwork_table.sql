CREATE TABLE artwork (
    id    INTEGER PRIMARY KEY,
    hash  INTEGER UNIQUE,
    image BLOB NOT NULL,
    thumb BLOB NOT NULL
);

ALTER TABLE album ADD COLUMN artwork_id INTEGER REFERENCES artwork(id);
-- 0 - embedded, 1 - cover, 2 - folder, 3 - front
ALTER TABLE album ADD COLUMN artwork_source INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN artwork_id INTEGER REFERENCES artwork(id);
ALTER TABLE track ADD COLUMN art_hash INTEGER;

INSERT INTO artwork (image, thumb)
    SELECT DISTINCT image, thumb FROM album WHERE image IS NOT NULL AND thumb IS NOT NULL;

-- lets migrated rows be found and hashed later
CREATE INDEX idx_artwork_migrated_image ON artwork(image) WHERE hash IS NULL;

UPDATE album SET artwork_id = (
    SELECT id FROM artwork
    WHERE artwork.hash IS NULL AND artwork.image = album.image AND artwork.thumb = album.thumb
)
WHERE image IS NOT NULL AND thumb IS NOT NULL;

ALTER TABLE album DROP COLUMN image;
ALTER TABLE album DROP COLUMN thumb;

CREATE INDEX idx_track_artwork_id ON track(artwork_id) WHERE artwork_id IS NOT NULL;
CREATE INDEX idx_album_artwork_id ON album(artwork_id) WHERE artwork_id IS NOT NULL;

CREATE TABLE scan_art (
    album_id INTEGER NOT NULL,
    hash     INTEGER NOT NULL,
    source   INTEGER NOT NULL,       -- same as artwork_source
    PRIMARY KEY (album_id, hash)
);
