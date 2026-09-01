DROP TRIGGER IF EXISTS update_track_album_adopt;

INSERT OR IGNORE INTO artist (name, name_sortable)
SELECT DISTINCT artist_name.value, artist_name.value
FROM track t, json_each(t.artists) AS artist_name
WHERE t.album_id IS NOT NULL
  AND TRIM(artist_name.value) != '';

INSERT OR IGNORE INTO track_artist (track_id, artist_id)
SELECT t.id, a.id
FROM track t, json_each(t.artists) AS artist_name
JOIN artist a ON a.name = artist_name.value
WHERE t.album_id IS NOT NULL
  AND TRIM(artist_name.value) != '';

INSERT OR IGNORE INTO artist (name, name_sortable)
SELECT DISTINCT t.artist_names, t.artist_names
FROM track t
WHERE t.album_id IS NOT NULL
  AND (t.artists IS NULL OR json_array_length(t.artists) = 0)
  AND t.artist_names IS NOT NULL
  AND TRIM(t.artist_names) != '';

INSERT OR IGNORE INTO track_artist (track_id, artist_id)
SELECT t.id, a.id
FROM track t
JOIN artist a ON a.name = t.artist_names
WHERE t.album_id IS NOT NULL
  AND (t.artists IS NULL OR json_array_length(t.artists) = 0)
  AND t.artist_names IS NOT NULL
  AND TRIM(t.artist_names) != '';
