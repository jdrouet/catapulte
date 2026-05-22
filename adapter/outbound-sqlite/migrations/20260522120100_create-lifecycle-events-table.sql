CREATE TABLE IF NOT EXISTS lifecycle_events (
    id BLOB PRIMARY KEY NOT NULL,
    email_id BLOB NOT NULL,
    event_type TEXT NOT NULL,
    payload JSON,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    FOREIGN KEY (email_id) REFERENCES emails(id)
);

CREATE INDEX IF NOT EXISTS lifecycle_events_email_id ON lifecycle_events(email_id);
CREATE INDEX IF NOT EXISTS lifecycle_events_created_at ON lifecycle_events(created_at);
