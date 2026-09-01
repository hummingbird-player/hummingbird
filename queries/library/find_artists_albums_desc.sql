SELECT a.id FROM artist a
LEFT JOIN album_artist aa ON aa.artist_id = a.id
WHERE EXISTS (
    SELECT 1 FROM album_artist visible_aa WHERE visible_aa.artist_id = a.id
) OR EXISTS (
    SELECT 1
    FROM track_artist visible_ta
    JOIN track visible_t ON visible_t.id = visible_ta.track_id
    WHERE visible_ta.artist_id = a.id AND visible_t.album_id IS NULL
)
GROUP BY a.id
ORDER BY COUNT(DISTINCT aa.album_id) DESC, a.name_sortable COLLATE NOCASE ASC;
