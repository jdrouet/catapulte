CREATE TABLE IF NOT EXISTS lifecycle_events (
    id UUID PRIMARY KEY NOT NULL,
    email_id UUID NOT NULL REFERENCES emails(id),
    event_type TEXT NOT NULL,
    payload JSONB,
    sender_name TEXT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT
);

CREATE INDEX IF NOT EXISTS lifecycle_events_email_id ON lifecycle_events(email_id);
CREATE INDEX IF NOT EXISTS lifecycle_events_sender_name
    ON lifecycle_events(sender_name) WHERE sender_name IS NOT NULL;
