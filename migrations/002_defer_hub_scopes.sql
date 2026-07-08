-- 002: Defer pending crawl scopes that were discovered by hub users
-- (following >= {HUB_THRESHOLD}).
--
-- Idempotent — already-low scopes remain low.  Only pending scopes are
-- affected; done / in_progress / error scopes are left unchanged.

UPDATE crawl_state
SET priority = 'low'
WHERE status = 'pending'
  AND scope_key IN (
    SELECT u2.login
    FROM edges e
    JOIN users u1 ON e.from_user_id = u1.id
    JOIN users u2 ON e.to_user_id = u2.id
    JOIN user_profiles up ON u1.id = up.user_id
    WHERE up.following >= {HUB_THRESHOLD}
      AND e.edge_type = 'follows'
  );
