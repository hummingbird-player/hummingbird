SELECT
    t.id,
    t.title_sortable,
    t.album_id,
    t.location, t.source, t.present
FROM
    track t
ORDER BY
    t.title_sortable COLLATE NOCASE DESC,
    t.album_id ASC,
    t.location COLLATE NOCASE ASC,
    t.id ASC;
