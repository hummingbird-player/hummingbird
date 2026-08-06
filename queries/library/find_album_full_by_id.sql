SELECT
    id,
    title,
    title_sortable,
    NULLIF(artist_display_override, '') AS artist_display_override,
    release_date,
    date_precision,
    created_at,
    image,
    label,
    catalog_number,
    isrc,
    vinyl_numbering
FROM album
WHERE id = $1;
