SELECT id, location
FROM track
WHERE id > $1
ORDER BY id ASC
LIMIT $2;
