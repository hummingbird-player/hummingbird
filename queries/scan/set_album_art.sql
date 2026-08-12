UPDATE album SET artwork_id = $1, artwork_source = $2
WHERE id = $3 AND artwork_id IS NULL;
