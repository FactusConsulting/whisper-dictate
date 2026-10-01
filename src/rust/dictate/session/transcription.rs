//! Ordered decode, dictionary, post-processing, formatting and injection phases.

use super::*;

enum TranscriptDecision {
    Ready(TranscribeResult, Vec<crate::dictionary::ReplacementChange>),
    Rejected(UtteranceOutcome),
}

impl<T: TranscribeBackend, I: InjectBackend> DictateSession<T, I> {
    /// Run the accepted utterance; recording owns the finally-equivalent Idle reset.
    pub(super) fn run_transcription<W: Write>(
        &mut self,
        writer: &mut W,
        buf: &[f32],
    ) -> Result<UtteranceOutcome, SessionError> {
        // Every attempt emits transcribing before checking for audio.
        wire::emit_status_with_output(
            writer,
            "transcribing",
            &self.capture_extras(),
            self.worker_event_output,
        )?;
        if buf.is_empty() {
            wire::emit_status_with_output(
                writer,
                "no_text",
                &[("reason", Value::from("no_audio"))],
                self.worker_event_output,
            )?;
            return Ok(UtteranceOutcome::NoAudio);
        }
        let recording_s = json!(wire::round2(buf.len() as f64 / SR as f64));
        let skip = crate::dictate::skip::should_skip(buf.len(), self.config.min_record_seconds);
        if let Some(reason) = skip.reason() {
            self.emit_no_text(writer, reason, recording_s.clone())?;
            return Ok(UtteranceOutcome::Skipped { reason });
        }
        match self.decode_transcript(writer, buf, &recording_s)? {
            TranscriptDecision::Ready(result, replacements) => {
                self.inject_transcript(writer, result, replacements, recording_s)
            }
            TranscriptDecision::Rejected(outcome) => Ok(outcome),
        }
    }

    fn decode_transcript<W: Write>(
        &mut self,
        writer: &mut W,
        buf: &[f32],
        recording_s: &Value,
    ) -> Result<TranscriptDecision, SessionError> {
        let mut result = match self.transcribe.transcribe(buf, SR) {
            Ok(result) => result,
            Err(err) => {
                wire::emit_status_with_output(
                    writer,
                    "no_text",
                    &[
                        ("reason", Value::from("no_speech")),
                        ("error", Value::from(err.to_string())),
                        ("recording_s", recording_s.clone()),
                    ],
                    self.worker_event_output,
                )?;
                return Ok(TranscriptDecision::Rejected(UtteranceOutcome::NoText {
                    reason: "no_speech",
                }));
            }
        };
        // Replacements precede classification: a dictionary may rescue a blacklist
        // phrase, or turn normal text into one. Keep unchanged backend verdicts.
        let replacements = self.rewrite_transcript(writer, &mut result)?;
        let reason = if result.text.is_empty() {
            Some(
                result
                    .gate
                    .as_deref()
                    .map(normalize_gate_reason)
                    .unwrap_or("empty"),
            )
        } else if result.is_hallucination {
            Some("no_speech")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.emit_no_text(writer, reason, recording_s.clone())?;
            return Ok(TranscriptDecision::Rejected(UtteranceOutcome::NoText {
                reason,
            }));
        }
        Ok(TranscriptDecision::Ready(result, replacements))
    }

    fn rewrite_transcript<W: Write>(
        &mut self,
        writer: &mut W,
        result: &mut TranscribeResult,
    ) -> Result<Vec<crate::dictionary::ReplacementChange>, SessionError> {
        let pre_dictionary_text = result.text.clone();
        let (dictated, replacements, dictionary_error) = self.apply_dictionary(&result.text);
        if let Some(error) = dictionary_error {
            wire::emit_status_with_output(
                writer,
                "dictionary_error",
                &[("error", Value::from(error))],
                self.worker_event_output,
            )?;
        }
        if dictated != result.text {
            result.is_hallucination = crate::dictate::backends::is_hallucination(dictated.trim());
        }
        result.text = dictated;
        // Preserve an existing raw backend copy; otherwise capture pre-replacement text.
        if result.raw_text.is_empty() {
            result.raw_text = pre_dictionary_text;
        }
        Ok(replacements)
    }

    fn emit_no_text<W: Write>(
        &self,
        writer: &mut W,
        reason: &'static str,
        recording_s: Value,
    ) -> Result<(), SessionError> {
        wire::emit_status_with_output(
            writer,
            "no_text",
            &[
                ("reason", Value::from(reason)),
                ("recording_s", recording_s),
            ],
            self.worker_event_output,
        )
    }

    fn prepare_transcript<W: Write>(
        &self,
        writer: &mut W,
        result: &TranscribeResult,
    ) -> Result<(String, Option<PostProcessOutcome>), SessionError> {
        let post = match self.post_process.as_ref() {
            Some(backend) if backend.is_active() => {
                wire::emit_status_with_output(
                    writer,
                    "post-processing",
                    &self.capture_extras(),
                    self.worker_event_output,
                )?;
                // Use dictionary-final text and the language actually used for this utterance.
                Some(backend.post_process(&result.text, &result.language))
            }
            _ => None,
        };
        let post_processed = post
            .as_ref()
            .map(|outcome| outcome.text.clone())
            .unwrap_or_else(|| result.text.clone());
        let text = crate::formatting::apply_format_commands(
            &post_processed,
            self.config.format_command_set.as_deref(),
        )
        .text;
        Ok((text, post))
    }

    fn inject_transcript<W: Write>(
        &mut self,
        writer: &mut W,
        result: TranscribeResult,
        replacements: Vec<crate::dictionary::ReplacementChange>,
        recording_s: Value,
    ) -> Result<UtteranceOutcome, SessionError> {
        let (text, post) = self.prepare_transcript(writer, &result)?;
        let dictionary_text = result.text.clone();
        let profile_name = self
            .active_profile
            .as_ref()
            .and_then(|profile| profile.name.clone());
        let window = self.active_window.clone();
        wire::emit_status_with_output(
            writer,
            "injecting",
            &self.capture_extras(),
            self.worker_event_output,
        )?;
        let inject_error = self
            .inject
            .prepare_target(window.as_ref())
            .and_then(|()| self.inject.inject(&text))
            .err()
            .map(|err| err.to_string());
        // Injection failure is observable but still records the attempted utterance.
        // Never retry here; the supervisor owns explicit retries.
        let payload = wire::emit_utterance_with_output(
            writer,
            &text,
            &result,
            recording_s,
            wire::UtterancePost {
                inject_error,
                post: post.as_ref(),
                replacements: &replacements,
            },
            wire::UtteranceExtras {
                dictionary_text: dictionary_text.as_str(),
                window: window.as_ref(),
                profile: profile_name.as_deref(),
                config: &self.config,
                audio_loss: self.recording_audio_loss,
            },
            wire::UtteranceEmission {
                run_command_hook: self.command_hook_enabled(),
                command_hook_settings: self.command_hook_settings(),
                output: self.worker_event_output,
            },
        )?;
        self.record_sinks(&payload);
        Ok(UtteranceOutcome::Injected { text, result })
    }

    fn record_sinks(&self, payload: &Value) {
        if let Some(sink) = self.history_sink.as_ref() {
            sink.append(payload);
        }
        if let Some(sink) = self.metrics_sink.as_ref() {
            sink.append(payload);
        }
    }
}
