SELECT art_hash FROM track
WHERE source = 'local' AND album_id = $1 AND art_hash IS NOT NULL
GROUP BY art_hash
ORDER BY COUNT(*) DESC, art_hash ASC LIMIT 1;
