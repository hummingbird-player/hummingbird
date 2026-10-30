CREATE TABLE sort_collation_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    locale TEXT NOT NULL
);

DROP INDEX IF EXISTS idx_artist_name_sortable;
CREATE INDEX idx_artist_name_sortable
    ON artist (name_sortable COLLATE HB_SORT, id);

DROP INDEX IF EXISTS idx_album_artist_sort;
CREATE INDEX idx_album_artist_sort
    ON album (artist_sort COLLATE HB_SORT, release_date);
