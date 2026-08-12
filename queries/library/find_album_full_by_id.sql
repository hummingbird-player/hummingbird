SELECT
    album.id,
    album.title,
    album.title_sortable,
    NULLIF(album.artist_display_override, '') AS artist_display_override,
    album.release_date,
    album.date_precision,
    album.created_at,
    artwork.image,
    album.label,
    album.catalog_number,
    album.isrc,
    album.vinyl_numbering
FROM album
LEFT JOIN artwork ON artwork.id = album.artwork_id
WHERE album.id = $1;
