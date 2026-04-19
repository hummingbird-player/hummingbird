SELECT
    playlist.id,
    playlist.name,
    playlist.created_at,
    playlist.type,
    playlist.position,
    COUNT(playlist_item.id) as track_count,
    COALESCE(SUM(track.duration), 0) as total_duration
FROM playlist
LEFT JOIN playlist_item ON playlist.id = playlist_item.playlist_id
LEFT JOIN track ON playlist_item.track_id = track.id
WHERE playlist.id = $1
GROUP BY playlist.id;
