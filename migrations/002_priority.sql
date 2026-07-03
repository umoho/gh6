-- v2: add priority column for crawl queue ordering
ALTER TABLE crawl_state ADD COLUMN priority TEXT DEFAULT 'normal';
CREATE INDEX IF NOT EXISTS idx_crawl_prio ON crawl_state(crawler_name, priority, status);
