INSERT INTO scan_art (album_id, hash, source)
    VALUES ($1, $2, $3)
    ON CONFLICT (album_id, hash) DO UPDATE SET source = CASE
        WHEN EXCLUDED.source = 0 THEN scan_art.source
        WHEN scan_art.source = 0 THEN EXCLUDED.source
        ELSE MIN(scan_art.source, EXCLUDED.source) END;
