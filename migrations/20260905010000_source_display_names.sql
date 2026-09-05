-- Derived display metadata only. Settings remain the editable configuration.
-- Retain the last known name when a connection is removed without a catalog purge.
ALTER TABLE library_source ADD COLUMN display_name TEXT;
