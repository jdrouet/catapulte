CREATE TABLE IF NOT EXISTS email_queue (
    id UUID PRIMARY KEY NOT NULL,
    email_id UUID NOT NULL REFERENCES emails(id),
    enqueued_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT,
    claimed_until BIGINT,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS email_queue_enqueued_at ON email_queue(enqueued_at);
CREATE INDEX IF NOT EXISTS email_queue_claimed_until ON email_queue(claimed_until);
