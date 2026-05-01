//! `SQLite` adapter for perima.

#![forbid(unsafe_code)]

pub mod cmd;
pub mod connection;
pub mod database_admin;
pub mod errors;
pub mod file_repo;
pub mod identity_cache_repo;
pub mod manifest;
pub mod metadata_repo;
pub mod pool;
pub mod schema;
pub mod search_repo;
pub mod tag_repo;
pub mod volume_repo;
pub mod writer;

pub use cmd::WriteCmd;
pub use connection::open_and_migrate;
pub use database_admin::SqliteDatabaseAdmin;
pub use errors::Error;
pub use file_repo::SqliteFileRepository;
pub use identity_cache_repo::SqliteIdentityCacheRepository;
pub use metadata_repo::SqliteMetadataRepository;
pub use pool::ReadPool;
pub use search_repo::SqliteSearchRepository;
pub use tag_repo::SqliteTagRepository;
pub use volume_repo::SqliteVolumeRepository;
pub use writer::{SqliteWriter, SqliteWriterHandle};

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
