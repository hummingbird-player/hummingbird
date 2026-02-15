CREATE TRIGGER IF NOT EXISTS update_track_album_cleanup AFTER UPDATE OF album_id ON track
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

    DELETE FROM artist
    WHERE NOT EXISTS (
        SELECT 1
        FROM album
        WHERE album.artist_id = artist.id
    );
END;
