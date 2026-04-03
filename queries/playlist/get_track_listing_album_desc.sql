SELECT pl.id, pl.track_id, t.album_id FROM playlist_item as pl
    JOIN track t on pl.track_id = t.id
    LEFT JOIN album al ON t.album_id = al.id
    WHERE pl.playlist_id = $1
    ORDER BY al.title_sortable COLLATE NOCASE DESC,
             t.disc_number DESC,
             t.track_number DESC;
