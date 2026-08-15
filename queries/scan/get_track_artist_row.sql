SELECT artists, artist_sort, album_artist_keys, artist_names FROM track WHERE id = $1 AND album_id IS NULL;
