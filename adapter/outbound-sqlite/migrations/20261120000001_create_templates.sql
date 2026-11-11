CREATE TABLE IF NOT EXISTS templates (
    name TEXT NOT NULL,
    version INTEGER NOT NULL,
    source TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (name, version)
);
