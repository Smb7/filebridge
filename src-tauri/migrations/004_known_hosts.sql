CREATE TABLE known_hosts (
    id TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    key_type TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    public_key BLOB NOT NULL,
    trusted_at TEXT NOT NULL,
    UNIQUE (host, port, key_type)
);
