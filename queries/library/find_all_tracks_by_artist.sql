SELECT t.*, 0 AS track_source,
       COALESCE(al.release_date, t.release_date) AS track_date,
       al.id AS track_album
FROM track t
JOIN album al ON t.album_id = al.id
JOIN album_artist aa ON aa.album_id = al.id
WHERE aa.artist_id = $1
UNION ALL
SELECT t.*, 1 AS track_source, t.release_date, t.id
FROM track t
JOIN track_artist ta ON ta.track_id = t.id
WHERE ta.artist_id = $1
ORDER BY track_source, track_date ASC, track_album ASC, disc_number ASC, track_number ASC;
