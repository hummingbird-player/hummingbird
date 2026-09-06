SELECT
    album_id,
    location
FROM
    track
WHERE
    source = 'local' AND album_id IS NOT NULL;
