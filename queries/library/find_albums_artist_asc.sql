SELECT
    p.id,
    p.title_sortable
FROM
    album p
ORDER BY
    p.artist_sort COLLATE NOCASE ASC,
    p.release_date ASC;
