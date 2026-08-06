SELECT a.id, a.name,
       (SELECT COUNT(*) FROM album_artist aa WHERE aa.artist_id = a.id) AS album_count,
       (SELECT COUNT(*) FROM track t JOIN album_artist aa ON t.album_id = aa.album_id WHERE aa.artist_id = a.id) AS track_count
FROM artist a
WHERE a.id = $1;
