SELECT
    track.id,
    track.title_sortable,
    track.album_id,
    track.location, track.source, track.present
FROM track
LEFT JOIN (
    SELECT DISTINCT
        track_genre.track_id,
        GROUP_CONCAT(genre.normalized_name, CHAR(31)) OVER (
            PARTITION BY track_genre.track_id
            ORDER BY track_genre.position
            ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
        ) AS genre_sort
    FROM track_genre
    JOIN genre ON genre.id = track_genre.genre_id
) AS genres ON genres.track_id = track.id
ORDER BY
    COALESCE(genres.genre_sort, '') COLLATE NOCASE DESC,
    track.title_sortable COLLATE NOCASE ASC,
    track.album_id ASC,
    track.location COLLATE NOCASE ASC,
    track.id ASC;
