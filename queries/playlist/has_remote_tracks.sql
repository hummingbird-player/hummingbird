SELECT EXISTS (
    SELECT 1 FROM playlist_item
    JOIN track ON track.id = playlist_item.track_id
    WHERE playlist_item.playlist_id = $1 AND track.source != 'local'
);
