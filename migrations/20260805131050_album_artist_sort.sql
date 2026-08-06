ALTER TABLE album ADD COLUMN artist_sort TEXT;
-- the raw ALBUMARTISTSORT tag, kept apart so artist_sort can be recomputed when tags change
ALTER TABLE album ADD COLUMN artist_sort_tag TEXT;

UPDATE album
SET artist_sort = COALESCE(
    (
        SELECT MIN(ar.name_sortable)
        FROM album_artist aa
        JOIN artist ar ON ar.id = aa.artist_id
        WHERE aa.album_id = album.id
    ),
    artist_display_override
);

CREATE INDEX idx_album_artist_sort ON album (artist_sort COLLATE NOCASE, release_date);
