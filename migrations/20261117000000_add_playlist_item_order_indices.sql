CREATE INDEX IF NOT EXISTS playlist_item_playlist_position
    ON playlist_item (playlist_id, position);
CREATE INDEX IF NOT EXISTS playlist_item_playlist_created_at
    ON playlist_item (playlist_id, created_at, position);
CREATE INDEX IF NOT EXISTS playlist_item_playlist_created_at_desc
    ON playlist_item (playlist_id, created_at DESC, position);
