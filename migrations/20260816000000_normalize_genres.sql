CREATE TABLE genre (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL UNIQUE
);

CREATE TABLE track_genre (
    track_id INTEGER NOT NULL,
    genre_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    FOREIGN KEY (track_id) REFERENCES track (id) ON DELETE CASCADE,
    FOREIGN KEY (genre_id) REFERENCES genre (id),
    PRIMARY KEY (track_id, genre_id)
);

CREATE TABLE album_genre (
    album_id INTEGER NOT NULL,
    genre_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    FOREIGN KEY (album_id) REFERENCES album (id) ON DELETE CASCADE,
    FOREIGN KEY (genre_id) REFERENCES genre (id),
    PRIMARY KEY (album_id, genre_id)
);

CREATE INDEX idx_track_genre_genre ON track_genre (genre_id, track_id);
CREATE INDEX idx_album_genre_genre ON album_genre (genre_id, album_id);

WITH legacy_genres AS (
    SELECT
        id,
        TRIM(genres) AS name,
        LOWER(TRIM(genres)) AS normalized_name
    FROM track
    WHERE genres IS NOT NULL
      AND TRIM(genres) != ''
),
first_genres AS (
    SELECT
        legacy.normalized_name,
        (
            SELECT candidate.name
            FROM legacy_genres candidate
            WHERE candidate.normalized_name = legacy.normalized_name
            ORDER BY candidate.id
            LIMIT 1
        ) AS name,
        MIN(legacy.id) AS first_track_id
    FROM legacy_genres legacy
    GROUP BY legacy.normalized_name
)
INSERT INTO genre (name, normalized_name)
SELECT name, normalized_name
FROM first_genres
ORDER BY first_track_id;

INSERT INTO track_genre (track_id, genre_id, position)
SELECT track.id, genre.id, 0
FROM track
JOIN genre ON genre.normalized_name = LOWER(TRIM(track.genres))
WHERE track.genres IS NOT NULL
  AND TRIM(track.genres) != '';

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
    WHERE track.album_id IS NOT NULL
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
FROM positioned_genres;

ALTER TABLE track DROP COLUMN genres;
