//! Runtime registry of constructed [`Transcriber`] impls, keyed by
//! [`BackendId`]. Built once at app startup from `TranscriptionConfig`.

use std::collections::HashMap;
use std::sync::Arc;

use perima_core::CoreError;
use perima_core::transcription::{BackendId, Transcriber, TranscriptionError};

/// Registry of available transcribers. The use-case looks up the active
/// backend by name and dispatches.
pub struct TranscriberRegistry {
    backends: HashMap<BackendId, Arc<dyn Transcriber>>,
    active: Option<BackendId>,
}

impl std::fmt::Debug for TranscriberRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY manual Debug: Arc<dyn Transcriber> is not Debug; elide backends map.
        f.debug_struct("TranscriberRegistry")
            .field("backend_count", &self.backends.len())
            .field("active", &self.active)
            .finish()
    }
}

impl TranscriberRegistry {
    /// Construct an empty registry. Use [`Self::register`] to add backends
    /// and [`Self::set_active`] to mark one as default.
    #[must_use]
    pub fn new() -> Self {
        Self {
            backends: HashMap::new(),
            active: None,
        }
    }

    /// Register a transcriber under its self-reported [`BackendId`].
    pub fn register(&mut self, backend: Arc<dyn Transcriber>) {
        let id = backend.id().clone();
        self.backends.insert(id, backend);
    }

    /// Mark a backend as the default. The [`BackendId`] must already be
    /// registered.
    ///
    /// # Errors
    /// Returns [`CoreError::Transcription`] wrapping
    /// [`TranscriptionError::BackendUnavailable`] if the id is not registered.
    pub fn set_active(&mut self, id: BackendId) -> Result<(), CoreError> {
        if !self.backends.contains_key(&id) {
            return Err(CoreError::Transcription(
                TranscriptionError::BackendUnavailable {
                    reason: format!("provider {id} is not registered"),
                },
            ));
        }
        self.active = Some(id);
        Ok(())
    }

    /// Get the active transcriber.
    ///
    /// # Errors
    /// Returns [`CoreError::Transcription`] wrapping
    /// [`TranscriptionError::BackendUnavailable`] if no active backend is configured.
    pub fn active(&self) -> Result<Arc<dyn Transcriber>, CoreError> {
        let id = self.active.as_ref().ok_or_else(|| {
            CoreError::Transcription(TranscriptionError::BackendUnavailable {
                reason: "no active transcription provider configured".to_owned(),
            })
        })?;
        let backend = self.backends.get(id).ok_or_else(|| {
            CoreError::Transcription(TranscriptionError::BackendUnavailable {
                reason: format!("active provider {id} disappeared from registry"),
            })
        })?;
        Ok(Arc::clone(backend))
    }

    /// Lookup a specific backend by id (used for `--provider` CLI override).
    #[must_use]
    pub fn get(&self, id: &BackendId) -> Option<Arc<dyn Transcriber>> {
        self.backends.get(id).map(Arc::clone)
    }
}

impl Default for TranscriberRegistry {
    fn default() -> Self {
        Self::new()
    }
}
