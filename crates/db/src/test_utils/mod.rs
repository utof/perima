//! Shared test utilities for `crates/db`.
//!
//! Gated on `#[cfg(any(test, feature = "test-utils"))]` from `lib.rs`
//! so production builds don't expose this module.

pub mod noop_bus;
pub use noop_bus::NoopBus;
