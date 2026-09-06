UPDATE album_path SET path = $1 WHERE path = $2
AND album_id IN (SELECT id FROM album WHERE source = 'local');
