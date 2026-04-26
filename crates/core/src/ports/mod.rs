//! Trait ports — the hexagonal boundary between core and adapters.

pub mod file_repo;
pub mod hash;
pub mod identity_cache;
pub mod metadata_repo;
pub mod scanner;
pub mod search_repo;
pub mod tag_repo;
pub mod volume_repo;

pub use file_repo::{BackfillFileRow, FileRepository};
pub use hash::HashService;
pub use identity_cache::{CacheEntry, CacheKey, IdentityCacheRepository};
pub use metadata_repo::MetadataRepository;
pub use scanner::{FileStat, Scanner};
pub use search_repo::SearchRepository;
pub use tag_repo::TagRepository;
pub use volume_repo::VolumeRepository;
