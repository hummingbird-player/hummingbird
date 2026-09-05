SELECT
    t.id,
    t.title_sortable,
    t.album_id,
    t.location, t.source, t.present
FROM
    track t
    LEFT JOIN album al ON t.album_id = al.id
ORDER BY
    al.title_sortable COLLATE NOCASE ASC,
    t.disc_number ASC,
    t.track_number ASC,
    t.track_section ASC;
