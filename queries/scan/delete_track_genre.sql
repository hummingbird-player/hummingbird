DELETE FROM track_genre
WHERE track_id = $1
  AND genre_id = $2;
