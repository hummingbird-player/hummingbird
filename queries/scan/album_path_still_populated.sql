SELECT DISTINCT folder FROM track
WHERE album_id = $1 AND IFNULL(disc_number, -1) = $2;
