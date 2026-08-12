SELECT DISTINCT art_hash FROM track
WHERE album_id = $1 AND art_hash IS NOT NULL AND art_hash != $2;
