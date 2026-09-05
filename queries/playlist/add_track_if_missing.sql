INSERT INTO playlist_item (playlist_id, track_id, position)
VALUES (
    $1,
    $2,
    COALESCE((SELECT position FROM playlist_item ORDER BY position DESC LIMIT 1) + 1, 1)
)
ON CONFLICT (playlist_id, track_id) DO NOTHING
