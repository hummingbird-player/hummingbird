SELECT a.id, a.name,
       (SELECT COUNT(*) FROM album_artist aa WHERE aa.artist_id = a.id) AS album_count,
       (SELECT COUNT(*)
        FROM (
            SELECT t.id
            FROM album_artist aa
            JOIN track t ON t.album_id = aa.album_id
            WHERE aa.artist_id = a.id
            UNION
            SELECT ta.track_id
            FROM track_artist ta
            WHERE ta.artist_id = a.id
        )) AS track_count
FROM artist a
WHERE a.id = $1;
