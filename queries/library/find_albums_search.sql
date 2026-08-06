SELECT
    p.id,
    p.title,
    NULLIF(p.artist_display_override, '') AS artist_display_override,
    IFNULL(GROUP_CONCAT(a.name, ' '), '') AS artists
FROM
    album p
    LEFT JOIN album_artist aa ON aa.album_id = p.id
    LEFT JOIN artist a ON a.id = aa.artist_id
GROUP BY p.id;
