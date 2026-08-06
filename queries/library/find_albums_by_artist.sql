SELECT al.id, al.title FROM album al
JOIN album_artist aa ON aa.album_id = al.id
WHERE aa.artist_id = $1
ORDER BY al.release_date ASC;
