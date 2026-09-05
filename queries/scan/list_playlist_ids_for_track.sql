SELECT DISTINCT playlist_id
FROM playlist_item
WHERE track_id IN (
    SELECT id FROM track WHERE source = 'local' AND location = $1
);
