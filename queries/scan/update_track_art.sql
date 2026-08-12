UPDATE track SET artwork_id = $1 WHERE id = $2 AND artwork_id IS NOT $1;
