SELECT id, location
FROM track
WHERE source = 'local' AND id > $1
ORDER BY id ASC
LIMIT $2;
