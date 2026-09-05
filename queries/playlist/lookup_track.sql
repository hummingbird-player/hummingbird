/*
 This query searches very very hard for a match for a given track. It's used to resolve items in M3U files.
 Parameters:
    1. The exact location of the track.
    2. The track title.
    3. The album artist's name.
    4. The album's title.
    5. The track artist names.
    6. The duration of the track, in seconds.
    7. A search string like $<FILE_NAME>$, to look up the path.
 */

SELECT t.id FROM track t
    LEFT JOIN main.album a ON a.id = t.album_id
    WHERE t.source = 'local' AND (t.location = $1
        OR (
                (    $2 IS NOT NULL
                 OR ($3 IS NOT NULL AND $7 IS NOT NULL)
                 OR ($4 IS NOT NULL AND $7 IS NOT NULL)
                 OR ($5 IS NOT NULL AND $7 IS NOT NULL)
                 OR ($6 IS NOT NULL AND $7 IS NOT NULL)
                )
            AND ($2 IS NULL OR t.title = $2)
            AND ($3 IS NULL OR a.artist_display_override = $3 OR EXISTS (
                    SELECT 1 FROM main.album_artist aa
                    JOIN main.artist a2 ON a2.id = aa.artist_id
                    WHERE aa.album_id = a.id AND a2.name = $3
                ))
            AND ($4 IS NULL OR a.title = $4)
            AND ($5 IS NULL OR t.artist_names = $5)
            AND ($6 IS NULL OR (t.duration > $6 - 1 AND t.duration < $6 + 1))
            AND ($7 IS NULL OR $2 IS NOT NULL OR t.location LIKE $7)
        ));
