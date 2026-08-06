SELECT a.id FROM artist a
LEFT JOIN album_artist aa ON aa.artist_id = a.id
GROUP BY a.id
ORDER BY COUNT(DISTINCT aa.album_id) ASC, a.name_sortable COLLATE NOCASE ASC;
