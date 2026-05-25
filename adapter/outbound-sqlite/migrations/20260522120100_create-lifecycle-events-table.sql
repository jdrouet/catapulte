CREATE TABLE IF NOT EXISTS lifecycle_events (
    id BLOB PRIMARY KEY NOT NULL,
    email_id BLOB NOT NULL,
    event_type TEXT NOT NULL,
    payload JSON,
    sender_name TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch('now', 'subsec') * 1000),
    FOREIGN KEY (email_id) REFERENCES emails(id)
);

CREATE INDEX IF NOT EXISTS lifecycle_events_email_id_created_at
    ON lifecycle_events(email_id, created_at);
CREATE INDEX IF NOT EXISTS lifecycle_events_created_at ON lifecycle_events(created_at);
CREATE INDEX IF NOT EXISTS lifecycle_events_event_type ON lifecycle_events(event_type);
CREATE INDEX IF NOT EXISTS lifecycle_events_sender_name
    ON lifecycle_events(sender_name) WHERE sender_name IS NOT NULL;
