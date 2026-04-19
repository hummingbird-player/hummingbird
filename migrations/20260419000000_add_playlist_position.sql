ALTER TABLE playlist ADD COLUMN position INTEGER NOT NULL DEFAULT 0;

UPDATE playlist
SET position = (
    SELECT COUNT(*)
    FROM playlist AS p2
    WHERE p2.created_at < playlist.created_at
       OR (p2.created_at = playlist.created_at AND p2.id <= playlist.id)
);

CREATE INDEX IF NOT EXISTS playlist_position ON playlist(position);
