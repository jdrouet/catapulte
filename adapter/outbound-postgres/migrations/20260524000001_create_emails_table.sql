CREATE TABLE IF NOT EXISTS emails (
    id UUID PRIMARY KEY NOT NULL,
    idempotency_key TEXT,
    subject TEXT,
    sender TEXT NOT NULL,
    recipients JSONB NOT NULL,
    body JSONB NOT NULL,
    variables JSONB NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW()) * 1000)::BIGINT
);

CREATE UNIQUE INDEX IF NOT EXISTS emails_idempotency_key
    ON emails(idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS emails_created_at ON emails(created_at);
