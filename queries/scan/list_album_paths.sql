SELECT album_path.path
FROM album_path JOIN album ON album.id = album_path.album_id
WHERE album.source = 'local' AND album_id = $1;
