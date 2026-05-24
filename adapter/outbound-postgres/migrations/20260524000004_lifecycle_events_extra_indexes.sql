CREATE INDEX IF NOT EXISTS lifecycle_events_email_id_created_at
    ON lifecycle_events(email_id, created_at);
CREATE INDEX IF NOT EXISTS lifecycle_events_event_type ON lifecycle_events(event_type);
