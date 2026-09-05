INSERT INTO album (title, title_sortable, artist_display_override, artist_sort, artist_sort_tag, release_date, date_precision, label, catalog_number, isrc, mbid, number_display_mode)
    VALUES ($1, $2, COALESCE($3, ''), $4, $4, $5, $6, $7, $8, $9, $10, $11)
    ON CONFLICT (title, artist_display_override, mbid) WHERE source = 'local' DO UPDATE SET
        title = EXCLUDED.title,
        title_sortable = EXCLUDED.title_sortable,
        artist_display_override = EXCLUDED.artist_display_override,
        artist_sort = COALESCE(EXCLUDED.artist_sort_tag, album.artist_sort),
        artist_sort_tag = EXCLUDED.artist_sort_tag,
        release_date = EXCLUDED.release_date,
        date_precision = EXCLUDED.date_precision,
        label = EXCLUDED.label,
        catalog_number = EXCLUDED.catalog_number,
        isrc = EXCLUDED.isrc,
        mbid = EXCLUDED.mbid,
        number_display_mode = EXCLUDED.number_display_mode
    RETURNING id;
