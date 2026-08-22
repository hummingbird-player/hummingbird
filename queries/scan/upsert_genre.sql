INSERT INTO genre (name, normalized_name)
VALUES ($1, $2)
ON CONFLICT (normalized_name) DO UPDATE SET
    normalized_name = EXCLUDED.normalized_name
RETURNING id;
