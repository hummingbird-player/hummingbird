SELECT location
FROM track
WHERE source = 'local' AND ((location >= $1 || '/' AND location < $1 || '0')
   OR (location >= $1 || '\' AND location < $1 || ']'));
