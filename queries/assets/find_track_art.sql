SELECT artwork.image
FROM track
LEFT JOIN artwork ON artwork.id = track.artwork_id
WHERE track.id = $1;
