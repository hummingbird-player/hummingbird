DELETE FROM artwork
WHERE id NOT IN (SELECT artwork_id FROM album WHERE artwork_id IS NOT NULL)
  AND id NOT IN (SELECT artwork_id FROM track WHERE artwork_id IS NOT NULL);
