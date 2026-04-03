SELECT pl.id, pl.track_id, t.album_id FROM playlist_item as pl
    JOIN track t on pl.track_id = t.id
    WHERE pl.playlist_id = $1
    ORDER BY t.title_sortable COLLATE NOCASE DESC;
