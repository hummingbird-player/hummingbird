SELECT t.* FROM track t
JOIN album al ON t.album_id = al.id
JOIN album_artist aa ON aa.album_id = al.id
JOIN playlist_item pi ON pi.track_id = t.id
WHERE aa.artist_id = $1 AND pi.playlist_id = 1
ORDER BY al.release_date DESC, al.id DESC, t.disc_number DESC, t.track_number DESC;
