CREATE TABLE track_artist (
    track_id INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    FOREIGN KEY (track_id) REFERENCES track (id) ON DELETE CASCADE,
    FOREIGN KEY (artist_id) REFERENCES artist (id),
    PRIMARY KEY (track_id, artist_id)
);

CREATE INDEX idx_track_artist_artist ON track_artist (artist_id, track_id);

DROP TRIGGER delete_artist_trigger;

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

-- tracks added to an album drop their artist links (for now)
CREATE TRIGGER update_track_album_adopt AFTER UPDATE OF album_id ON track
WHEN OLD.album_id IS NULL AND NEW.album_id IS NOT NULL
BEGIN
    DELETE FROM track_artist
    WHERE track_artist.track_id = NEW.id;
END;
