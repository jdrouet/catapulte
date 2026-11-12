ALTER TABLE email_queue
    ALTER COLUMN claimed_until SET DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT;

UPDATE email_queue SET claimed_until = enqueued_at WHERE claimed_until IS NULL;

ALTER TABLE email_queue ALTER COLUMN claimed_until SET NOT NULL;
