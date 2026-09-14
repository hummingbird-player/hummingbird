ALTER TABLE artist DROP COLUMN bio;
ALTER TABLE artist DROP COLUMN image;
ALTER TABLE artist DROP COLUMN image_mime;
ALTER TABLE artist DROP COLUMN tags;

ALTER TABLE album DROP COLUMN tags;

ALTER TABLE track DROP COLUMN tags;
ALTER TABLE track DROP COLUMN rg_track_gain;
ALTER TABLE track DROP COLUMN rg_track_peak;
ALTER TABLE track DROP COLUMN rg_album_gain;
ALTER TABLE track DROP COLUMN rg_album_peak;
