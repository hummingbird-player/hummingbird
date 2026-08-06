SELECT id FROM album WHERE title = $1 AND mbid = $2 AND artist_display_override = COALESCE($3, '');
