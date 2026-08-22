WITH ordered_genres AS (
    SELECT
        track.album_id,
        track_genre.genre_id,
        ROW_NUMBER() OVER (
            PARTITION BY track.album_id
            ORDER BY
                COALESCE(track.disc_number, 2147483647),
                COALESCE(track.track_number, 2147483647),
                track.id,
                track_genre.position
        ) AS source_position
    FROM track
    JOIN track_genre ON track_genre.track_id = track.id
    WHERE track.album_id = $1
),
first_occurrences AS (
    SELECT album_id, genre_id, MIN(source_position) AS source_position
    FROM ordered_genres
    GROUP BY album_id, genre_id
),
positioned_genres AS (
    SELECT
        album_id,
        genre_id,
        ROW_NUMBER() OVER (
            PARTITION BY album_id
            ORDER BY source_position, genre_id
        ) - 1 AS position
    FROM first_occurrences
)
INSERT INTO album_genre (album_id, genre_id, position)
SELECT album_id, genre_id, position
FROM positioned_genres
WHERE TRUE
ON CONFLICT (album_id, genre_id) DO UPDATE SET
    position = EXCLUDED.position;
