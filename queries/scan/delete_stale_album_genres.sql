DELETE FROM album_genre
WHERE album_id = $1
  AND NOT EXISTS (
      SELECT 1
      FROM track
      JOIN track_genre ON track_genre.track_id = track.id
      WHERE track.album_id = $1
        AND track_genre.genre_id = album_genre.genre_id
  );
