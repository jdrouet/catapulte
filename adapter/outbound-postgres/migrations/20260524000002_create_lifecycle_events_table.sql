CREATE TABLE IF NOT EXISTS lifecycle_events (
    id UUID PRIMARY KEY NOT NULL,
    email_id UUID NOT NULL REFERENCES emails(id),
    event_type TEXT NOT NULL,
    payload JSONB,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT
);

CREATE INDEX IF NOT EXISTS lifecycle_events_email_id ON lifecycle_events(email_id);
