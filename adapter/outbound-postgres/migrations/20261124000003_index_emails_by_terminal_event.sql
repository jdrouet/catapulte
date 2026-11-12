CREATE INDEX IF NOT EXISTS emails_api_key_terminal_created_at
    ON emails(api_key, terminal_event_type, created_at DESC, id DESC)
    WHERE cancelled_at_ms IS NULL;
CREATE INDEX IF NOT EXISTS emails_terminal_created_at
    ON emails(terminal_event_type, created_at DESC, id DESC)
    WHERE cancelled_at_ms IS NULL;
CREATE INDEX IF NOT EXISTS emails_api_key_cancelled_created_at
    ON emails(api_key, created_at DESC, id DESC)
    WHERE cancelled_at_ms IS NOT NULL;
CREATE INDEX IF NOT EXISTS emails_cancelled_created_at
    ON emails(created_at DESC, id DESC)
    WHERE cancelled_at_ms IS NOT NULL;
CREATE INDEX IF NOT EXISTS emails_created_at_id ON emails(created_at DESC, id DESC);
DROP INDEX IF EXISTS emails_created_at;
