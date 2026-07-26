SELECT
    album_id,
    location
FROM
    track
WHERE
    album_id IS NOT NULL;
