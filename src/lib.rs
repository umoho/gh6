pub mod db;
pub mod types;

/// Following threshold above which a user is considered a hub and their
/// discovered scopes inherit `low` priority.
pub const HUB_FOLLOWING_THRESHOLD: i64 = 5000;
