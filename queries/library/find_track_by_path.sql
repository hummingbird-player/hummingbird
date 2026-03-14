SELECT * FROM track
WHERE location = $1
LIMIT 1;
