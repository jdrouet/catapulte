CREATE TABLE IF NOT EXISTS email_queue (
    id BLOB NOT NULL PRIMARY KEY,
    email_id BLOB NOT NULL REFERENCES emails(id),
    enqueued_at INTEGER NOT NULL DEFAULT (unixepoch('now', 'subsec') * 1000),
    claimed_until INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS email_queue_enqueued_at ON email_queue(enqueued_at);
CREATE INDEX IF NOT EXISTS email_queue_claimed_until ON email_queue(claimed_until);
