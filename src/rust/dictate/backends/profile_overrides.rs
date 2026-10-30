//! Per-utterance profile override slots shared by every production
//! `TranscribeBackend`. The session calls
//! `TranscribeBackend::apply_profile_overrides` before each `transcribe`;
//! the backend forwards the matched profile's settings here and reads the
//! resolved prompt / language / guards back out when it builds the request.
//!
//! Slot semantics:
//!
//! - `initial_prompt`: profile wins over the reload-prompt fold and the
//!   fixed config prompt. A blank / whitespace-only value clears the
//!   override (matching the settings-schema treatment of empty as unset).
//! - `language` / `lang`: tri-state. Absent keeps the startup
//!   configuration; an explicit blank value selects Auto; a non-blank
//!   value is the trimmed hint. The outer `Option` on the slot carries
//!   that distinction so Danish -> Auto is not mistaken for "unchanged".
//! - `model`: deferred. The model is fixed at construction, so a
//!   non-blank value prints one `model_change_deferred` warning per new
//!   value; an absent value clears the dedupe slot so a later profile that
//!   re-introduces the same `model=X` warns exactly once again.
//! - Speech-gate / rate-limit keys fold into the in-process
//!   [`TranscriptionGuards`] when the runtime owns them.
//! - Dictionary keys fold into the attached reloading prompt provider.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::hallucination::TranscriptionGuards;
use crate::dictionary::ReloadingDictionary;

pub(crate) struct ProfileOverrideSlots {
    /// Backend-specific explanation appended to the deferred-model warning.
    model_change_note: &'static str,
    /// Live-reloading STT prompt provider. When set, the base prompt is
    /// re-folded with the live dictionary terms on every `transcribe`.
    /// Boxed so a backend without one stays small.
    prompt_reload: Option<Box<Mutex<ReloadingDictionary>>>,
    prompt: Mutex<Option<String>>,
    /// `Arc` so a preview wrapper can share the live selection.
    language: Arc<Mutex<Option<Option<String>>>>,
    model_warned: Mutex<Option<String>>,
    /// `Arc` so a preview wrapper can share the live thresholds.
    guards: Arc<Mutex<Option<TranscriptionGuards>>>,
}

impl ProfileOverrideSlots {
    pub(crate) fn new(model_change_note: &'static str) -> Self {
        Self {
            model_change_note,
            prompt_reload: None,
            prompt: Mutex::new(None),
            language: Arc::new(Mutex::new(None)),
            model_warned: Mutex::new(None),
            guards: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn set_prompt_reload(&mut self, reload: ReloadingDictionary) {
        self.prompt_reload = Some(Box::new(Mutex::new(reload)));
    }

    /// The attached reloading prompt provider (Nemotron backend tests).
    #[cfg(all(test, feature = "nemotron-local"))]
    pub(crate) fn prompt_reload(&self) -> Option<&Mutex<ReloadingDictionary>> {
        self.prompt_reload.as_deref()
    }

    /// Bind speech-gate and rate-limit settings to this in-process session.
    #[cfg(any(
        test,
        feature = "nemotron-local",
        all(feature = "whisper-rs-local", feature = "rust-injection")
    ))]
    pub(crate) fn set_guards(&self, guards: TranscriptionGuards) {
        *self.guards.lock().unwrap_or_else(|p| p.into_inner()) = Some(guards);
    }

    /// Guards for the next utterance: the session-owned values when bound,
    /// else the process environment.
    pub(crate) fn guards(&self) -> TranscriptionGuards {
        self.guards
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or_else(TranscriptionGuards::from_env)
    }

    /// Live thresholds handle for the whisper.cpp preview wrapper.
    #[cfg(any(test, feature = "whisper-rs-local"))]
    pub(crate) fn shared_guards(&self) -> Arc<Mutex<Option<TranscriptionGuards>>> {
        Arc::clone(&self.guards)
    }

    /// Live language-selection handle for the whisper.cpp preview wrapper.
    #[cfg(any(test, feature = "whisper-rs-local"))]
    pub(crate) fn shared_language(&self) -> Arc<Mutex<Option<Option<String>>>> {
        Arc::clone(&self.language)
    }

    /// Raw tri-state language slot: `None` = no override applied,
    /// `Some(None)` = explicit Auto, `Some(Some(code))` = explicit hint.
    pub(crate) fn language(&self) -> Option<Option<String>> {
        self.language
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The language hint for the next utterance: the profile slot when an
    /// override was supplied (explicit Auto included), else `configured`.
    pub(crate) fn language_or(&self, configured: Option<String>) -> Option<String> {
        self.language().unwrap_or(configured)
    }

    /// The STT prompt for this utterance plus the dictionary terms folded
    /// into it. Precedence (highest first): profile override (no terms),
    /// reload-prompt fold over `base`, then `base` verbatim with
    /// `static_terms`.
    pub(crate) fn prompt(
        &self,
        base: Option<&str>,
        static_terms: &[String],
    ) -> (Option<String>, Vec<String>) {
        if let Some(profile) = self
            .prompt
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return (Some(profile), Vec::new());
        }
        match &self.prompt_reload {
            Some(reload) => reload
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .initial_prompt_with_terms(base),
            None => (base.map(str::to_owned), static_terms.to_vec()),
        }
    }

    #[cfg(test)]
    pub(crate) fn warned_model(&self) -> Option<String> {
        self.model_warned
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn apply(&self, settings: &BTreeMap<String, String>) {
        if let Some(guards) = self
            .guards
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            guards.apply_settings(settings);
        }
        if let Some(reload) = self.prompt_reload.as_ref() {
            crate::dictionary::DictionaryProvider::apply_settings(
                &mut *reload.lock().unwrap_or_else(|p| p.into_inner()),
                settings,
            );
        }
        *self.prompt.lock().unwrap_or_else(|p| p.into_inner()) =
            non_blank(settings.get("initial_prompt"));
        let language_supplied = settings.contains_key("language") || settings.contains_key("lang");
        *self.language.lock().unwrap_or_else(|p| p.into_inner()) = language_supplied
            .then(|| non_blank(settings.get("language").or_else(|| settings.get("lang"))));
        let mut warned = self.model_warned.lock().unwrap_or_else(|p| p.into_inner());
        match non_blank(settings.get("model")) {
            Some(model) => {
                if warned.as_deref() != Some(model.as_str()) {
                    crate::diag::log!(
                        "[profile] model_change_deferred model={} restart_needed=true \
                         ({}; restart the app for a `model` profile override to take effect)",
                        model,
                        self.model_change_note
                    );
                    *warned = Some(model);
                }
            }
            None => *warned = None,
        }
    }
}

fn non_blank(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "profile_overrides_tests.rs"]
mod tests;
