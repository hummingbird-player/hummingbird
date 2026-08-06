SELECT artists, artist_sort, album_artist_keys FROM track WHERE album_id = $1 ORDER BY id;
