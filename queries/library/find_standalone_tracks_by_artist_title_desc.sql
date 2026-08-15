SELECT t.*, 0 AS track_source, t.release_date AS track_date, t.id AS track_album
FROM track t
JOIN track_artist ta ON ta.track_id = t.id
WHERE ta.artist_id = $1 AND t.album_id IS NULL
ORDER BY title_sortable DESC;
