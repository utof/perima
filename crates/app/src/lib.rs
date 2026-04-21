//! perima-app — application-service layer.
//!
//! Five concrete `UseCase` structs orchestrate across the `perima-core`
//! ports. Each exposes a single `async fn execute(&self, cmd: Cmd) ->
//! Result<Out, CoreError>`. Zero generic parameters; dependency ports
//! carried as `Arc<dyn Port>` fields. CLI and Desktop shells consume
//! the `AppContainer` built from an `AppDeps`.
//!
//! # Why this shape
//!
//! Audit §4.1: mature Rust codebases (zed, rust-analyzer, atuin,
//! crates.io) use concrete orchestrator structs with `Arc<dyn Port>`
//! fields, not trait-object soup or `struct UseCase<R1, R2, R3>`.
//! LLM-authoring sessions reproduce this shape with the highest
//! fidelity.
//!
//! # Watch deferral
//!
//! `Watch` is intentionally NOT a `UseCase` in v1. The long-running +
//! cancellation-handle shape doesn't fit `async fn execute(&self, cmd)
//! -> Result<Out, CoreError>`. Watch stays in `crates/cli/src/cmd/
//! watch.rs` + `crates/desktop/src/commands.rs::{start_watch,
//! stop_watch, is_watching}` until a dedicated design lands (see
//! follow-up GH issue filed during Batch B).

#![forbid(unsafe_code)]

pub mod scan;
pub mod search;

pub use scan::{
    FullScan, METADATA_DRAIN_TIMEOUT, OnPersist, ScanCommand, ScanReport, ScanReportEntry,
    ScanUseCase,
};
pub use search::{SearchCommand, SearchOutput, SearchUseCase};
