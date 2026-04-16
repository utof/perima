//! `SQLite` adapter for perima.

pub mod connection;
pub mod errors;
pub mod file_repo;

pub use connection::open_and_migrate;
pub use errors::Error;
pub use file_repo::SqliteFileRepository;
