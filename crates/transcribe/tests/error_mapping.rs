//! Snapshot tests for the HTTP-error → `TranscriptionError` mapping.
//!
//! Each test constructs a representative `async-openai` error and asserts
//! the mapped [`perima_core::transcription::TranscriptionError`] variant.
//! Mapping is the seam between the upstream HTTP wire format and the
//! typed error surface the use-case + frontend rely on; a careless
//! implementer flipping arms here would degrade UX silently. Cover all
//! 5 spec'd discriminants (`Auth`, `RateLimited`, `QuotaExceeded`,
//! `ModelNotFound`, `BackendUnavailable`) explicitly.

#![allow(clippy::unwrap_used)]

use async_openai::error::{ApiError, OpenAIError};

use perima_core::CoreError;
use perima_core::transcription::{BackendId, TranscriptionError};
use perima_transcribe::openai_compat::map_async_openai_error;

const FILE_LIMIT_BYTES: u64 = 25_000_000;

fn backend() -> BackendId {
    BackendId("test:model-x".to_owned())
}

fn api_err_with_code(code: &str, message: &str) -> OpenAIError {
    OpenAIError::ApiError(ApiError {
        message: message.to_owned(),
        r#type: None,
        param: None,
        code: Some(code.to_owned()),
    })
}

#[test]
fn auth_error_maps_to_auth_variant() {
    let err = api_err_with_code("invalid_api_key", "Incorrect API key provided");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    assert!(matches!(
        mapped,
        CoreError::Transcription(TranscriptionError::Auth)
    ));
}

#[test]
fn rate_limit_maps_to_rate_limited_with_no_retry_after() {
    let err = api_err_with_code("rate_limit_exceeded", "Rate limit exceeded");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    match mapped {
        CoreError::Transcription(TranscriptionError::RateLimited { retry_after_secs }) => {
            // async-openai 0.36's ApiError has no header surface — see
            // map_api_error WHY-block. Assert None explicitly so any future
            // change that starts populating retry_after gets a test signal.
            assert_eq!(retry_after_secs, None);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn quota_maps_to_quota_exceeded() {
    let err = api_err_with_code("quota_exceeded", "billing limit hit");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    assert!(matches!(
        mapped,
        CoreError::Transcription(TranscriptionError::QuotaExceeded)
    ));
}

#[test]
fn model_not_found_maps_with_backend_and_model_strings() {
    let err = api_err_with_code("model_not_found", "no such model");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    match mapped {
        CoreError::Transcription(TranscriptionError::ModelNotFound { backend, model }) => {
            assert_eq!(backend, "test:model-x");
            assert_eq!(model, "model-x");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn unknown_api_error_maps_to_backend_unavailable_with_message_and_code() {
    let err = api_err_with_code("server_error", "internal server error");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    match mapped {
        CoreError::Transcription(TranscriptionError::BackendUnavailable { reason }) => {
            assert!(reason.contains("internal server error"), "got {reason}");
            assert!(reason.contains("server_error"), "got {reason}");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn billing_hard_limit_maps_to_quota_exceeded() {
    // Sibling code that should also classify as QuotaExceeded — covers the
    // |-or arm in map_api_error so a later split into separate variants
    // doesn't silently regress.
    let err = api_err_with_code("billing_hard_limit_reached", "you are out of credits");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    assert!(matches!(
        mapped,
        CoreError::Transcription(TranscriptionError::QuotaExceeded)
    ));
}

#[test]
fn unauthorized_code_also_maps_to_auth() {
    // OpenAI sometimes returns "unauthorized" instead of "invalid_api_key";
    // both must collapse to Auth so frontend UX is consistent.
    let err = api_err_with_code("unauthorized", "Missing or invalid API key");
    let mapped = map_async_openai_error(err, &backend(), "model-x", FILE_LIMIT_BYTES);
    assert!(matches!(
        mapped,
        CoreError::Transcription(TranscriptionError::Auth)
    ));
}
