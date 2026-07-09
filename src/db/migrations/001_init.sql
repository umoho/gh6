-- v3: stable layer + extension layer
--
-- Discards v1/v2 data. Delete ~/.local/share/gh6/gh6.db before restart.

-- ============================================
-- 稳定层
-- ============================================

CREATE TABLE IF NOT EXISTS users (
    id             INTEGER PRIMARY KEY,
    login          TEXT NOT NULL UNIQUE,
    discovered_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS edges (
    from_user_id   INTEGER NOT NULL REFERENCES users(id),
    to_user_id     INTEGER NOT NULL REFERENCES users(id),
    edge_type      TEXT NOT NULL,
    weight         REAL DEFAULT 1.0,
    degree         INTEGER,
    metadata       TEXT,
    is_active      INTEGER NOT NULL DEFAULT 1,
    first_seen_at  TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen_at   TEXT NOT NULL DEFAULT (datetime('now')),
    removed_at     TEXT,
    PRIMARY KEY (from_user_id, to_user_id, edge_type)
);

-- ============================================
-- 扩展层
-- ============================================

CREATE TABLE IF NOT EXISTS user_profiles (
    user_id        INTEGER PRIMARY KEY REFERENCES users(id),
    name           TEXT,
    avatar_url     TEXT,
    company        TEXT,
    location       TEXT,
    followers      INTEGER NOT NULL,
    following      INTEGER NOT NULL,
    public_repos   INTEGER NOT NULL,
    created_at     TEXT,
    updated_at     TEXT,
    fetched_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS edge_history (
    id             INTEGER PRIMARY KEY,
    from_user_id   INTEGER NOT NULL,
    to_user_id     INTEGER NOT NULL,
    edge_type      TEXT NOT NULL,
    action         TEXT NOT NULL,
    recorded_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS crawl_state (
    crawler_name   TEXT NOT NULL,
    scope_key      TEXT NOT NULL,
    status         TEXT DEFAULT 'pending',
    priority       TEXT DEFAULT 'normal',
    degree         INTEGER,
    error_count    INTEGER DEFAULT 0,
    last_error     TEXT,
    crawled_at     TEXT,
    PRIMARY KEY (crawler_name, scope_key)
);

CREATE TABLE IF NOT EXISTS config (
    key            TEXT PRIMARY KEY,
    value          TEXT NOT NULL
);
