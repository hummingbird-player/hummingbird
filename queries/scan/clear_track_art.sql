UPDATE track SET artwork_id = NULL WHERE source = 'local' AND album_id = $1;
