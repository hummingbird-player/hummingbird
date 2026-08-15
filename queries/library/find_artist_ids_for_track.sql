SELECT id, name FROM (
    SELECT a.id, a.name, a.name_sortable FROM artist a
    JOIN album_artist aa ON aa.artist_id = a.id
    JOIN track t ON t.album_id = aa.album_id
    WHERE t.id = $1
    UNION
    SELECT a.id, a.name, a.name_sortable FROM artist a
    JOIN track_artist ta ON ta.artist_id = a.id
    WHERE ta.track_id = $1
)
ORDER BY name_sortable COLLATE NOCASE ASC;
