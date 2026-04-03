SELECT pl.id, pl.track_id, t.album_id FROM playlist_item as pl
    JOIN track t on pl.track_id = t.id
    LEFT JOIN album al ON t.album_id = al.id
    LEFT JOIN artist ar ON al.artist_id = ar.id
    WHERE pl.playlist_id = $1
    ORDER BY ar.name_sortable COLLATE NOCASE ASC,
             al.title_sortable COLLATE NOCASE ASC,
             t.disc_number ASC,
             t.track_number ASC;
