SELECT
    p.id,
    p.title_sortable
FROM
    album p
ORDER BY
    p.artist_sort COLLATE NOCASE DESC,
    p.release_date ASC;
