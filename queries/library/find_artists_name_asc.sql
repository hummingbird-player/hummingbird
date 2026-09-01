SELECT a.id
FROM artist a
WHERE EXISTS (
    SELECT 1 FROM album_artist aa WHERE aa.artist_id = a.id
) OR EXISTS (
    SELECT 1
    FROM track_artist ta
    JOIN track t ON t.id = ta.track_id
    WHERE ta.artist_id = a.id AND t.album_id IS NULL
)
ORDER BY a.name_sortable COLLATE NOCASE ASC;
