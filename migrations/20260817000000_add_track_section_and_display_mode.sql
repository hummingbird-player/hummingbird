ALTER TABLE track ADD COLUMN track_section INTEGER;
ALTER TABLE track ADD COLUMN number_display_mode_hint INTEGER NOT NULL DEFAULT 0;
ALTER TABLE album ADD COLUMN number_display_mode INTEGER NOT NULL DEFAULT 0;
UPDATE album SET number_display_mode = vinyl_numbering;
UPDATE track
SET number_display_mode_hint = 1
WHERE album_id IN (SELECT id FROM album WHERE vinyl_numbering);
ALTER TABLE album DROP COLUMN vinyl_numbering;
