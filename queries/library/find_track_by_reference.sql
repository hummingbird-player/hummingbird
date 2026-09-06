SELECT * FROM track
WHERE source = $1 AND location = $2
LIMIT 1;
