//! Trait ports — the hexagonal boundary between core and adapters.

pub mod file_repo;
pub mod hash;
pub mod scanner;
pub mod volume_repo;

pub use file_repo::FileRepository;
pub use hash::HashService;
pub use scanner::Scanner;
pub use volume_repo::VolumeRepository;
