SELECT artwork.thumb
FROM album
LEFT JOIN artwork ON artwork.id = album.artwork_id
WHERE album.id = $1;
