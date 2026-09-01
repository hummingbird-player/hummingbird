SELECT
    id,
    title,
    title_sortable,
    NULLIF(artist_display_override, '') AS artist_display_override,
    release_date,
    date_precision,
    created_at,
    label,
    catalog_number,
    isrc,
    number_display_mode
FROM album
WHERE id = $1;
