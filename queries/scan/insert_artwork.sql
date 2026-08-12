INSERT INTO artwork (hash, image, thumb) VALUES ($1, $2, $3) RETURNING id;
