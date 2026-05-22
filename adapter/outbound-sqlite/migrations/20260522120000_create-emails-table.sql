CREATE TABLE IF NOT EXISTS emails (
    id BLOB PRIMARY KEY NOT NULL,
    idempotency_key TEXT,
    sender TEXT NOT NULL,
    recipients JSON NOT NULL,
    body JSON NOT NULL,
    variables JSON NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE INDEX IF NOT EXISTS emails_idempotency_key ON emails(idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS emails_created_at ON emails(created_at);
