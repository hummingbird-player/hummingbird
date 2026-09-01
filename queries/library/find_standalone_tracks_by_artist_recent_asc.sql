SELECT t.*, 0 AS track_source, t.release_date AS track_date, t.id AS track_album
FROM track t
JOIN track_artist ta ON ta.track_id = t.id
WHERE ta.artist_id = $1
  AND NOT EXISTS (
      SELECT 1 FROM album_artist aa
      WHERE aa.album_id = t.album_id AND aa.artist_id = $1
  )
ORDER BY created_at ASC;
