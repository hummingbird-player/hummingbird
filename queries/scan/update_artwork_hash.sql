-- TODO(2026-08): remove with the migrated-artwork adoption path in artwork.rs.
UPDATE artwork SET hash = $1 WHERE id = $2;
