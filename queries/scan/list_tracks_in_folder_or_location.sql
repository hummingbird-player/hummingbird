SELECT location
FROM track
WHERE folder = $1 OR location = $1;
