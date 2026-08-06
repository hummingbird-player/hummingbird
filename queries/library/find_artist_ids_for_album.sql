SELECT a.id, a.name FROM artist a
JOIN album_artist aa ON aa.artist_id = a.id
WHERE aa.album_id = $1
ORDER BY a.name_sortable COLLATE NOCASE ASC;
