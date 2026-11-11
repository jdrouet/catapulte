UPDATE email_queue SET claimed_until = enqueued_at WHERE claimed_until IS NULL;

CREATE TRIGGER IF NOT EXISTS email_queue_claimed_until_default AFTER INSERT ON email_queue
WHEN NEW.claimed_until IS NULL
BEGIN
    UPDATE email_queue SET claimed_until = NEW.enqueued_at WHERE id = NEW.id;
END;
