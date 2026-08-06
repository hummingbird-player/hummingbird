SELECT t.* FROM track t
JOIN album al ON t.album_id = al.id
JOIN album_artist aa ON aa.album_id = al.id
WHERE aa.artist_id = $1
ORDER BY al.release_date ASC,
         al.id ASC, t.disc_number ASC, t.track_number ASC;
