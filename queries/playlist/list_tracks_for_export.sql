SELECT t.location AS location, t.duration AS duration, t.artist_names AS track_artist_names, a.artist_display_override AS artist_name, t.title as track_title, a.title AS album_title
    FROM playlist_item AS pl
    JOIN track t ON pl.track_id = t.id
    JOIN album a ON t.album_id = a.id
    WHERE pl.playlist_id = $1
    ORDER BY pl.position;
