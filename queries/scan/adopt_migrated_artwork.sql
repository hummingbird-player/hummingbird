-- TODO(2026-08): remove with the migrated-artwork adoption path in artwork.rs.
SELECT id FROM artwork WHERE hash IS NULL AND image = $1;
