SELECT location
FROM track
WHERE (location >= $1 || '/' AND location < $1 || '0')
   OR (location >= $1 || '\' AND location < $1 || ']');
