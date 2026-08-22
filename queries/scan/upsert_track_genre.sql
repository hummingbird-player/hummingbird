INSERT INTO track_genre (track_id, genre_id, position)
VALUES ($1, $2, $3)
ON CONFLICT (track_id, genre_id) DO UPDATE SET
    position = EXCLUDED.position;
