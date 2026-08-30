SELECT
    t.id,
    t.title_sortable,
    t.album_id,
    t.location
FROM
    track t
    LEFT JOIN album al ON t.album_id = al.id
ORDER BY
    al.title_sortable COLLATE NOCASE DESC,
    t.disc_number ASC,
    t.track_number ASC,
    t.track_section ASC;
