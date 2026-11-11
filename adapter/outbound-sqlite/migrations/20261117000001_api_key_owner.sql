ALTER TABLE emails ADD COLUMN api_key TEXT NOT NULL DEFAULT '';
DROP INDEX IF EXISTS emails_idempotency_key;
CREATE UNIQUE INDEX emails_owner_idempotency_key ON emails(api_key, idempotency_key) WHERE idempotency_key IS NOT NULL;
