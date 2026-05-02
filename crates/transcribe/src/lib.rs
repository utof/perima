//! Transcription adapters for perima.
//!
//! - [`audio`] — ffmpeg-based audio extraction shim (`AudioPipeline` trait).
//! - [`providers`] — preset table for known OpenAI-compatible providers.
//! - [`registry`] — runtime registry of constructed [`perima_core::transcription::Transcriber`] impls.
//! - [`openai_compat`] — single cloud adapter wrapping `async-openai` for all
//!   `OpenAI` `/v1/audio/transcriptions`-compatible providers.
//!
//! See spec `docs/superpowers/specs/2026-05-02-transcription-v1-design.md`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod audio;
pub mod openai_compat;
pub mod providers;
pub mod registry;
