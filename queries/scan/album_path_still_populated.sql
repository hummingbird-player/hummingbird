SELECT DISTINCT folder FROM track
WHERE source = 'local' AND album_id = $1 AND IFNULL(disc_number, -1) = $2;
