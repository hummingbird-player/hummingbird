UPDATE track SET artwork_id = $1
WHERE source = 'local' AND album_id = $2 AND (art_hash IS NULL OR art_hash = $3) AND artwork_id IS NOT $1;
