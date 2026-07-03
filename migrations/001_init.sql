-- v1
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    login         TEXT NOT NULL UNIQUE,
    name          TEXT,
    avatar_url    TEXT,
    company       TEXT,
    location      TEXT,
    followers     INTEGER,
    following     INTEGER,
    public_repos  INTEGER,
    created_at    TEXT,
    updated_at    TEXT
);

CREATE TABLE IF NOT EXISTS edges (
    from_user_id   INTEGER NOT NULL REFERENCES users(id),
    to_user_id     INTEGER NOT NULL REFERENCES users(id),
    edge_type      TEXT NOT NULL,
    weight         REAL DEFAULT 1.0,
    degree         INTEGER,
    metadata       TEXT,
    discovered_at  TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (from_user_id, to_user_id, edge_type)
);

CREATE TABLE IF NOT EXISTS crawl_state (
    crawler_name   TEXT NOT NULL,
    scope_key      TEXT NOT NULL,
    status         TEXT DEFAULT 'pending',
    last_error     TEXT,
    crawled_at     TEXT,
    PRIMARY KEY (crawler_name, scope_key)
);
