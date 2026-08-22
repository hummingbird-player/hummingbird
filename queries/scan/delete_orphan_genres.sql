DELETE FROM genre
WHERE NOT EXISTS (
    SELECT 1
    FROM track_genre
    WHERE track_genre.genre_id = genre.id
)
AND NOT EXISTS (
    SELECT 1
    FROM album_genre
    WHERE album_genre.genre_id = genre.id
);
