INSERT INTO playlist (name, position)
    VALUES($1, COALESCE((SELECT MAX(position) FROM playlist) + 1, 1));
