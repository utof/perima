//! `UUIDv7` helpers.
//!
//! `UUIDv7` (RFC 9562) is time-sortable with 48-bit ms timestamps; it
//! gives us globally unique IDs whose B-tree insertion order matches
//! creation order, avoiding `SQLite` index fragmentation.

use uuid::Uuid;

/// Generate a fresh `UUIDv7`.
#[must_use]
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}
