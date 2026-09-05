SELECT
    album_id,
    location, source, present
FROM
    track
WHERE
    album_id IS NOT NULL;
