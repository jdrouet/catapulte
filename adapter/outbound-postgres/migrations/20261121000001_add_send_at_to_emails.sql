ALTER TABLE emails ADD COLUMN IF NOT EXISTS send_at_ms BIGINT;
CREATE INDEX IF NOT EXISTS email_queue_email_id ON email_queue(email_id);
