SELECT hash, source FROM scan_art
WHERE album_id = $1 AND source > 0
ORDER BY source ASC, hash ASC LIMIT 1;
