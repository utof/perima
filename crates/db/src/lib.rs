//! `SQLite` adapter for perima.

pub mod connection;
pub mod errors;
pub mod file_repo;
pub mod manifest;
pub mod volume_repo;

pub use connection::open_and_migrate;
pub use errors::Error;
pub use file_repo::SqliteFileRepository;
pub use volume_repo::SqliteVolumeRepository;
