//! Trait ports — the hexagonal boundary between core and adapters.

pub mod file_repo;
pub mod hash;
pub mod metadata_repo;
pub mod scanner;
pub mod tag_repo;
pub mod volume_repo;

pub use file_repo::FileRepository;
pub use hash::HashService;
pub use metadata_repo::MetadataRepository;
pub use scanner::Scanner;
pub use tag_repo::TagRepository;
pub use volume_repo::VolumeRepository;
