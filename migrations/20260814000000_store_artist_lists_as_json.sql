WITH RECURSIVE artist_parts(id, part, rest) AS (
    SELECT id, '', artists || ';'
    FROM track
    WHERE artists IS NOT NULL AND TRIM(artists) != ''
    UNION ALL
    SELECT id,
           TRIM(SUBSTR(rest, 1, INSTR(rest, ';') - 1)),
           SUBSTR(rest, INSTR(rest, ';') + 1)
    FROM artist_parts
    WHERE rest != ''
),
artist_lists AS (
    SELECT id, JSON_GROUP_ARRAY(part) AS list
    FROM artist_parts
    WHERE part != ''
    GROUP BY id
)
UPDATE track
SET artists = (SELECT list FROM artist_lists WHERE artist_lists.id = track.id)
WHERE id IN (SELECT id FROM artist_lists);

WITH RECURSIVE key_parts(id, part, rest) AS (
    SELECT id, '', album_artist_keys || ';'
    FROM track
    WHERE album_artist_keys IS NOT NULL AND TRIM(album_artist_keys) != ''
    UNION ALL
    SELECT id,
           TRIM(SUBSTR(rest, 1, INSTR(rest, ';') - 1)),
           SUBSTR(rest, INSTR(rest, ';') + 1)
    FROM key_parts
    WHERE rest != ''
),
key_lists AS (
    SELECT id, JSON_GROUP_ARRAY(part) AS list
    FROM key_parts
    WHERE part != ''
    GROUP BY id
)
UPDATE track
SET album_artist_keys = (SELECT list FROM key_lists WHERE key_lists.id = track.id)
WHERE id IN (SELECT id FROM key_lists);
