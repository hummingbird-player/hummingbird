UPDATE artist
SET name_sortable = name
WHERE EXISTS (
    SELECT 1
    FROM album_artist aa
    JOIN track t ON t.album_id = aa.album_id
    WHERE aa.artist_id = artist.id
      AND (t.artist_sort = artist.name_sortable OR t.artist_names = artist.name_sortable)
)
AND NOT EXISTS (
    SELECT 1
    FROM album_artist aa
    JOIN track t ON t.album_id = aa.album_id
    WHERE aa.artist_id = artist.id
      AND (t.artist_names = artist.name OR t.artists LIKE '%' || artist.name || '%')
);

UPDATE album
SET artist_sort = COALESCE(
    artist_sort_tag,
    (
        SELECT MIN(ar.name_sortable)
        FROM album_artist aa
        JOIN artist ar ON ar.id = aa.artist_id
        WHERE aa.album_id = album.id
    ),
    artist_display_override
);
