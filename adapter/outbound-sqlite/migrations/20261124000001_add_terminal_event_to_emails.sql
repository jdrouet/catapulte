ALTER TABLE emails ADD COLUMN terminal_event_type TEXT NOT NULL DEFAULT '';
ALTER TABLE emails ADD COLUMN terminal_event_at_ms INTEGER;
ALTER TABLE emails ADD COLUMN terminal_event_id BLOB;
