//! Transcription adapters for perima.
//!
//! - [`audio`] — ffmpeg-based audio extraction shim (`AudioPipeline` trait).
//! - [`providers`] — preset table for known OpenAI-compatible providers.
//! - [`registry`] — runtime registry of constructed [`perima_core::transcription::Transcriber`] impls.
//!
//! See spec `docs/superpowers/specs/2026-05-02-transcription-v1-design.md`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod audio;
pub mod providers;
pub mod registry;

// Adapter impls land in T4.
// pub mod openai_compat;
