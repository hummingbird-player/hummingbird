CREATE TABLE playlist_item_new (
    id INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL,
    track_id INTEGER NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    position INTEGER NOT NULL,
    FOREIGN KEY (playlist_id) REFERENCES playlist(id) ON DELETE CASCADE,
    FOREIGN KEY (track_id) REFERENCES track(id) ON DELETE CASCADE
);

INSERT INTO playlist_item_new (id, playlist_id, track_id, created_at, position)
SELECT id, playlist_id, track_id, created_at, position
FROM playlist_item
WHERE track_id IN (SELECT id FROM track)
    AND playlist_id IN (SELECT id FROM playlist);

DROP TABLE playlist_item;

ALTER TABLE playlist_item_new RENAME TO playlist_item;

CREATE UNIQUE INDEX IF NOT EXISTS playlist_item_playlist_id_track_id ON playlist_item(playlist_id, track_id);

CREATE TABLE lyrics_new (
    track_id INTEGER PRIMARY KEY,
    content TEXT NOT NULL,
    FOREIGN KEY (track_id) REFERENCES track (id) ON DELETE CASCADE
);

INSERT INTO lyrics_new (track_id, content)
SELECT track_id, content
FROM lyrics
WHERE track_id IN (SELECT id FROM track);

DROP TABLE lyrics;

ALTER TABLE lyrics_new RENAME TO lyrics;
