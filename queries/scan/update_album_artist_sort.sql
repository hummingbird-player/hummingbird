UPDATE album
SET artist_sort = COALESCE(
    artist_sort_tag,
    (
        SELECT MIN(ar.name_sortable)
        FROM album_artist aa
        JOIN artist ar ON ar.id = aa.artist_id
        WHERE aa.album_id = $1
    ),
    artist_display_override
)
WHERE id = $1
  AND artist_sort IS NOT COALESCE(
    artist_sort_tag,
    (
        SELECT MIN(ar.name_sortable)
        FROM album_artist aa
        JOIN artist ar ON ar.id = aa.artist_id
        WHERE aa.album_id = $1
    ),
    artist_display_override
);
