SELECT artists, artist_sort, album_artist_keys, artist_names FROM track WHERE album_id = $1 ORDER BY id;
