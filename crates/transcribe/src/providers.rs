//! Preset table for known OpenAI-compatible STT providers.
//!
//! See spec § "The cloud adapter — `OpenAICompatibleTranscriber`".

/// Auth scheme for an HTTP-based STT backend.
#[derive(Debug, Clone, Copy)]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// Custom header (e.g. `api-key: <key>` for Azure).
    XApiKey {
        /// The header name to use (e.g. `"api-key"`).
        header: &'static str,
    },
    /// No auth (used for local self-hosted servers in dev).
    None,
}

/// Static preset for a known provider.
#[derive(Debug, Clone, Copy)]
pub struct ProviderPreset {
    /// Stable provider name used in `BackendId` and config TOML.
    pub name: &'static str,
    /// Default API base URL.
    pub base_url: &'static str,
    /// Default model name when user doesn't override.
    pub default_model: &'static str,
    /// Backend's file-size ceiling in bytes (audio post-remux must fit).
    pub file_size_limit_bytes: u64,
    /// Auth header shape.
    pub auth_scheme: AuthScheme,
}

/// All bundled providers. Adding a new provider = one entry here.
///
/// See spec § "The cloud adapter" for the rationale (one OpenAI-compat
/// adapter + `base_url` covers all known servers in 2026).
pub const KNOWN_PROVIDERS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "whisper-large-v3-turbo",
        // WHY 24 MiB not 25 MiB: Groq's stated limit is 25 MiB but their
        // parser rejects files at exactly the limit due to multipart overhead.
        // A 1 MiB headroom eliminates the edge case without meaningful loss.
        file_size_limit_bytes: 24 * 1024 * 1024,
        auth_scheme: AuthScheme::Bearer,
    },
    ProviderPreset {
        name: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "whisper-1",
        file_size_limit_bytes: 24 * 1024 * 1024,
        auth_scheme: AuthScheme::Bearer,
    },
    ProviderPreset {
        name: "custom",
        base_url: "",      // user supplies
        default_model: "", // user supplies
        file_size_limit_bytes: 24 * 1024 * 1024,
        auth_scheme: AuthScheme::Bearer,
    },
];

/// Lookup a preset by name.
#[must_use]
pub fn find_preset(name: &str) -> Option<&'static ProviderPreset> {
    KNOWN_PROVIDERS.iter().find(|p| p.name == name)
}
