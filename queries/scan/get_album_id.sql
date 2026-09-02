SELECT id
FROM album
WHERE title = $1
  AND mbid = $2
  AND (
      ($4 IS NOT NULL AND id = $4)
      OR mbid != 'none'
      OR artist_display_override = COALESCE($3, '')
  );
