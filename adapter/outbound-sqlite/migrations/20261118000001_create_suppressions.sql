CREATE TABLE IF NOT EXISTS suppressions (
    address TEXT PRIMARY KEY NOT NULL,
    reason TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    source_email_id BLOB
);

CREATE INDEX IF NOT EXISTS suppressions_reason ON suppressions(reason);
