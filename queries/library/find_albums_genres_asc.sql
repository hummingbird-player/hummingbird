SELECT
    album.id,
    album.title_sortable
FROM album
LEFT JOIN (
    SELECT DISTINCT
        album_genre.album_id,
        GROUP_CONCAT(genre.normalized_name, CHAR(31)) OVER (
            PARTITION BY album_genre.album_id
            ORDER BY album_genre.position
            ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
        ) AS genre_sort
    FROM album_genre
    JOIN genre ON genre.id = album_genre.genre_id
) AS genres ON genres.album_id = album.id
ORDER BY
    COALESCE(genres.genre_sort, '') COLLATE NOCASE ASC,
    album.title_sortable COLLATE NOCASE ASC,
    album.id ASC;
