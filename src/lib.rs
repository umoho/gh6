pub mod db;
pub mod types;

/// Following threshold above which a user is considered an outbound hub
/// and their discovered scopes inherit `low` priority.
pub const HUB_FOLLOWING_THRESHOLD: i64 = 5000;

/// Followers threshold above which a user is considered an inbound hub
/// and their discovered scopes inherit `low` priority.
pub const HUB_FOLLOWER_THRESHOLD: i64 = 5000;
