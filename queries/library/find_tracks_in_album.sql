SELECT * FROM track
WHERE album_id = $1
ORDER BY disc_number ASC, track_number ASC, track_section ASC;
