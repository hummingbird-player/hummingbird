SELECT album_path.album_id, album_path.path
FROM album_path JOIN album ON album.id = album_path.album_id
WHERE album.source = 'local' AND disc_num IN (-1, 0, 1);
