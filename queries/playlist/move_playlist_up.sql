UPDATE playlist SET position = position + 1 WHERE position >= $1 AND position < $2;
UPDATE playlist SET position = $1 WHERE id = $3;
