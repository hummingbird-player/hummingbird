CREATE INDEX IF NOT EXISTS idx_artist_name_sortable
    ON artist (name_sortable COLLATE NOCASE, id);
