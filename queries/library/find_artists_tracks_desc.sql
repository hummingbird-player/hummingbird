SELECT a.id FROM artist a
LEFT JOIN album_artist aa ON aa.artist_id = a.id
LEFT JOIN track t ON t.album_id = aa.album_id
LEFT JOIN track_artist ta ON ta.artist_id = a.id
GROUP BY a.id
ORDER BY COUNT(DISTINCT t.id) + COUNT(DISTINCT ta.track_id) DESC,
         a.name_sortable COLLATE NOCASE ASC;
