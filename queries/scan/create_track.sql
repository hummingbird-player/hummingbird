INSERT INTO track (title, title_sortable, album_id, track_number, disc_number, duration, location, genres, artist_names, folder, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, disc_subtitle, artists, artist_sort, album_artist_keys, art_hash)
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)
    ON CONFLICT (location) DO UPDATE SET
        title = EXCLUDED.title,
        title_sortable = EXCLUDED.title_sortable,
        album_id = EXCLUDED.album_id,
        track_number = EXCLUDED.track_number,
        disc_number = EXCLUDED.disc_number,
        duration = EXCLUDED.duration,
        location = EXCLUDED.location,
        genres = EXCLUDED.genres,
        artist_names = EXCLUDED.artist_names,
        folder = EXCLUDED.folder,
        rg_track_gain = EXCLUDED.rg_track_gain,
        rg_track_peak = EXCLUDED.rg_track_peak,
        rg_album_gain = EXCLUDED.rg_album_gain,
        rg_album_peak = EXCLUDED.rg_album_peak,
        disc_subtitle = EXCLUDED.disc_subtitle,
        artists = EXCLUDED.artists,
        artist_sort = EXCLUDED.artist_sort,
        album_artist_keys = EXCLUDED.album_artist_keys,
        art_hash = EXCLUDED.art_hash
    RETURNING id;
