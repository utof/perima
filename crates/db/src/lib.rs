//! `SQLite` adapter for perima.

pub mod connection;
pub mod errors;
pub mod file_repo;
pub mod manifest;
pub mod metadata_repo;
pub mod search_repo;
pub mod tag_repo;
pub mod volume_repo;

pub use connection::open_and_migrate;
pub use errors::Error;
pub use file_repo::SqliteFileRepository;
pub use metadata_repo::SqliteMetadataRepository;
pub use search_repo::SqliteSearchRepository;
pub use tag_repo::SqliteTagRepository;
pub use volume_repo::SqliteVolumeRepository;
