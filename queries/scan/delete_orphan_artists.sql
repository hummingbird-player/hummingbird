DELETE FROM artist WHERE NOT EXISTS (
    SELECT 1 FROM album_artist WHERE album_artist.artist_id = artist.id
) AND NOT EXISTS (
    SELECT 1 FROM track_artist WHERE track_artist.artist_id = artist.id
);
