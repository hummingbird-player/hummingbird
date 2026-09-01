WITH artist_tracks AS (
    SELECT t.id AS track_id
    FROM album_artist aa
    JOIN track t ON t.album_id = aa.album_id
    WHERE aa.artist_id = $1
    UNION
    SELECT ta.track_id
    FROM track_artist ta
    WHERE ta.artist_id = $1
)
SELECT t.*, CASE WHEN t.album_id IS NULL THEN 1 ELSE 0 END AS track_source,
       COALESCE(al.release_date, t.release_date) AS track_date,
       COALESCE(al.id, t.id) AS track_album
FROM artist_tracks artist_t
JOIN track t ON t.id = artist_t.track_id
LEFT JOIN album al ON t.album_id = al.id
ORDER BY track_source, track_date ASC, track_album ASC, disc_number ASC, track_number ASC,
         track_section ASC;
