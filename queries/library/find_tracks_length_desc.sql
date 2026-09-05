SELECT
    t.id,
    t.title_sortable,
    t.album_id,
    t.location, t.source, t.present
FROM
    track t
ORDER BY
    t.duration DESC;
