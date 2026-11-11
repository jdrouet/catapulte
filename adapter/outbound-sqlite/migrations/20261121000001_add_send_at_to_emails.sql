ALTER TABLE emails ADD COLUMN send_at_ms INTEGER;
CREATE INDEX IF NOT EXISTS email_queue_email_id ON email_queue(email_id);
