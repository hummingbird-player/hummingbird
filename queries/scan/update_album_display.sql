UPDATE album
SET artist_display_override = COALESCE($2, ''),
    number_display_mode = $3
WHERE id = $1;
