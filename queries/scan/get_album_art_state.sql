SELECT al.artwork_id, al.artwork_source, aw.hash
FROM album al
LEFT JOIN artwork aw ON aw.id = al.artwork_id
WHERE al.id = $1;
