CREATE INDEX IF NOT EXISTS emails_api_key_created_at ON emails(api_key, created_at DESC, id DESC);
