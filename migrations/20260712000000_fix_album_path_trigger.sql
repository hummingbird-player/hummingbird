DROP TRIGGER IF EXISTS delete_album_path_trigger;

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
