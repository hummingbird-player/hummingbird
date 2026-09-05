SELECT * FROM track
WHERE source = 'local' AND location = $1
LIMIT 1;
