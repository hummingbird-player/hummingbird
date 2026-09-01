WITH visible_artists AS (
    SELECT artist_id FROM album_artist
    UNION
    SELECT ta.artist_id
    FROM track_artist ta
    JOIN track t ON t.id = ta.track_id
    WHERE t.album_id IS NULL
),
artist_tracks AS (
    SELECT aa.artist_id, t.id AS track_id
    FROM album_artist aa
    JOIN track t ON t.album_id = aa.album_id
    UNION
    SELECT artist_id, track_id FROM track_artist
),
track_counts AS (
    SELECT artist_id, COUNT(*) AS track_count
    FROM artist_tracks
    GROUP BY artist_id
)
SELECT a.id
FROM visible_artists visible_a
JOIN artist a ON a.id = visible_a.artist_id
LEFT JOIN track_counts counts ON counts.artist_id = a.id
ORDER BY COALESCE(counts.track_count, 0) ASC,
         a.name_sortable COLLATE NOCASE ASC;
