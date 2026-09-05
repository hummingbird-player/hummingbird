SELECT location
FROM track
WHERE source = 'local' AND (folder = $1 OR location = $1);
