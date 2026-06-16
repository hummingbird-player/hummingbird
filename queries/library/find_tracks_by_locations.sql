SELECT
    t.location,
    t.id,
    t.album_id,
    pi.id
FROM track t
LEFT JOIN playlist_item pi ON pi.track_id = t.id AND pi.playlist_id = ?
WHERE t.location IN ({})
