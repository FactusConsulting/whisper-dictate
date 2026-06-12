//! Parsing + display model + localized strings for the "record a corpus item
//! from the app" feature (System tab).
//!
//! Kept self-contained (its own localized strings, parser, and display model)
//! so the feature can be added/removed without touching the over-500-line
//! `text.rs` enum that parallel PRs also edit. The worker
//! (`--record-corpus-item <id>`) prints newline-delimited JSON events on stdout:
//!
//!   * `corpus_record_start`    — id, the reference text, the computed seconds,
//!   * `corpus_record_progress` — a countdown `remaining_s` (streamed to the log),
//!   * `corpus_record_done`     — id, saved path, seconds_recorded, peak/rms dBFS,
//!   * `corpus_record_error`    — a short error string.
//!
//! The background task captures the whole stdout and hands it here once finished;
//! [`parse_corpus_record_result`] scans it for the terminal done/error event and
//! turns it into a [`CorpusRecordOutcome`] the System tab renders inline (a green
//! "Saved …" confirmation or a red error), mirroring the device-test result row.

use serde::Deserialize;

/// Localized strings for the corpus-record UI. Self-contained (not added to the
/// shared `UiTextKey` enum) so this feature stays mergeable alongside parallel
/// `text.rs` edits. EN + DA are both provided so the picker/button/status read
/// natively in either UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum CorpusRecordText {
    /// Section label for the corpus-record cluster.
    SectionLabel,
    /// Hover/help for the whole cluster.
    SectionHelp,
    /// Label preceding the item picker.
    PickerLabel,
    /// The "Record" button.
    RecordButton,
    /// Hover/help for the Record button.
    RecordButtonHelp,
    /// Instruction shown above the reference text the user reads aloud.
    ReadAloudPrompt,
    /// Hint shown when recording is blocked because the runtime is running.
    StopRuntimeHint,
    /// Status while the worker records.
    Recording,
    /// Placeholder when no corpus item is available to record.
    NoItems,
    /// Prefix for the inline "saved" confirmation (followed by the path/seconds).
    Saved,
    /// Marker appended to picker entries that already have a recording.
    RecordedMarker,
}

impl CorpusRecordText {
    fn label(self, danish: bool) -> &'static str {
        if danish {
            self.danish()
        } else {
            self.english()
        }
    }

    fn english(self) -> &'static str {
        match self {
            Self::SectionLabel => "Record corpus audio",
            Self::SectionHelp => {
                "Record reference audio for a benchmark corpus item from your microphone: \
                 pick an item, read the shown text aloud after clicking Record, and the clip \
                 is saved where the benchmark scores it. Stop the dictation runtime first."
            }
            Self::PickerLabel => "Corpus item",
            Self::RecordButton => "Record",
            Self::RecordButtonHelp => {
                "Records you reading the reference text aloud and saves it as this item's \
                 benchmark audio. Uses the same microphone as dictation. Blocked while the \
                 runtime is running or another task is in progress."
            }
            Self::ReadAloudPrompt => "Read this aloud when recording starts:",
            Self::StopRuntimeHint => "Stop the dictation runtime to record corpus audio.",
            Self::Recording => "Recording — read the text aloud…",
            Self::NoItems => "No corpus items found to record.",
            Self::Saved => "Saved",
            Self::RecordedMarker => "recorded",
        }
    }

    fn danish(self) -> &'static str {
        match self {
            Self::SectionLabel => "Optag korpus-lyd",
            Self::SectionHelp => {
                "Optag reference-lyd til et benchmark-korpuselement fra din mikrofon: vælg et \
                 element, læs den viste tekst højt efter du har klikket Optag, og klippet \
                 gemmes der hvor benchmarken scorer det. Stop dikterings-runtimen først."
            }
            Self::PickerLabel => "Korpuselement",
            Self::RecordButton => "Optag",
            Self::RecordButtonHelp => {
                "Optager dig læse reference-teksten højt og gemmer den som dette elements \
                 benchmark-lyd. Bruger samme mikrofon som diktering. Blokeret mens runtimen \
                 kører eller en anden opgave er i gang."
            }
            Self::ReadAloudPrompt => "Læs dette højt når optagelsen starter:",
            Self::StopRuntimeHint => "Stop dikterings-runtimen for at optage korpus-lyd.",
            Self::Recording => "Optager — læs teksten højt…",
            Self::NoItems => "Ingen korpuselementer fundet at optage.",
            Self::Saved => "Gemt",
            Self::RecordedMarker => "optaget",
        }
    }
}

/// Resolve a corpus-record string for the active UI language. Mirrors the raw
/// `"da"` → Danish mapping used everywhere else (any other value is English),
/// kept local so this module stays self-contained for clean parallel merges.
pub(in crate::ui) fn corpus_record_text(raw_language: &str, key: CorpusRecordText) -> &'static str {
    key.label(raw_language == "da")
}

/// The terminal `corpus_record_done` event fields the UI surfaces.
#[derive(Debug, Clone, Deserialize)]
struct DoneEvent {
    #[serde(default)]
    id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    seconds_recorded: f64,
    #[serde(default)]
    peak_dbfs: Option<f64>,
}

/// The parsed terminal outcome of a record run, rendered inline next to the
/// Record button. `Saved` → green confirmation with the saved path + duration;
/// `Failed` → red error message.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) enum CorpusRecordOutcome {
    Saved {
        id: String,
        path: String,
        seconds_recorded: f64,
        peak_dbfs: Option<f64>,
    },
    Failed {
        error: String,
    },
}

/// Scan the worker's captured stdout for the terminal `corpus_record_done` /
/// `corpus_record_error` event and turn it into a [`CorpusRecordOutcome`].
///
/// The stdout is newline-delimited JSON (possibly preceded by log lines and the
/// start/progress events), so each line is tried in turn and the LAST terminal
/// event wins. A run that produced no terminal event (worker crashed before
/// emitting one) yields `Err` so the UI shows that rather than silently nothing.
pub(in crate::ui) fn parse_corpus_record_result(
    stdout: &str,
) -> Result<CorpusRecordOutcome, String> {
    let mut outcome: Option<CorpusRecordOutcome> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        match value.get("event").and_then(serde_json::Value::as_str) {
            Some("corpus_record_done") => {
                if let Ok(done) = serde_json::from_value::<DoneEvent>(value) {
                    outcome = Some(CorpusRecordOutcome::Saved {
                        id: done.id,
                        path: done.path,
                        seconds_recorded: done.seconds_recorded,
                        peak_dbfs: done.peak_dbfs,
                    });
                }
            }
            Some("corpus_record_error") => {
                let error = value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("recording failed")
                    .to_owned();
                outcome = Some(CorpusRecordOutcome::Failed { error });
            }
            _ => {}
        }
    }
    outcome.ok_or_else(|| "no recording result found in worker output".to_owned())
}

/// A plain, language-agnostic one-line summary of a record outcome for the
/// runtime log (the localized inline rendering lives in the System tab).
pub(in crate::ui) fn corpus_record_log_detail(outcome: &CorpusRecordOutcome) -> String {
    match outcome {
        CorpusRecordOutcome::Saved {
            id,
            path,
            seconds_recorded,
            peak_dbfs,
        } => {
            let peak = peak_dbfs
                .map(|db| format!(", peak {db:.1} dBFS"))
                .unwrap_or_default();
            format!("saved {id} → {path} ({seconds_recorded:.1}s{peak})")
        }
        CorpusRecordOutcome::Failed { error } => format!("failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_done_event_into_saved_outcome() {
        let stdout = "whisper-dictate 1.10.0\n\
            {\"event\":\"corpus_record_start\",\"id\":\"da-001\",\"text\":\"Hej\",\"seconds\":10.0}\n\
            {\"event\":\"corpus_record_progress\",\"remaining_s\":5}\n\
            {\"event\":\"corpus_record_done\",\"id\":\"da-001\",\"path\":\"/a/da-001.wav\",\"seconds_recorded\":9.8,\"peak_dbfs\":-6.0,\"rms_dbfs\":-20.0}\n";
        let outcome = parse_corpus_record_result(stdout).unwrap();
        assert_eq!(
            outcome,
            CorpusRecordOutcome::Saved {
                id: "da-001".to_owned(),
                path: "/a/da-001.wav".to_owned(),
                seconds_recorded: 9.8,
                peak_dbfs: Some(-6.0),
            }
        );
    }

    #[test]
    fn parses_error_event_into_failed_outcome() {
        let stdout =
            "{\"event\":\"corpus_record_start\",\"id\":\"x\",\"text\":\"t\",\"seconds\":10}\n\
            {\"event\":\"corpus_record_error\",\"error\":\"unknown corpus id: x\"}\n";
        let outcome = parse_corpus_record_result(stdout).unwrap();
        assert_eq!(
            outcome,
            CorpusRecordOutcome::Failed {
                error: "unknown corpus id: x".to_owned()
            }
        );
    }

    #[test]
    fn last_terminal_event_wins() {
        // Defensive: a done followed by an error reports the error (the later one).
        let stdout = "{\"event\":\"corpus_record_done\",\"id\":\"a\",\"path\":\"p\",\"seconds_recorded\":1.0}\n\
            {\"event\":\"corpus_record_error\",\"error\":\"late failure\"}\n";
        let outcome = parse_corpus_record_result(stdout).unwrap();
        assert!(matches!(outcome, CorpusRecordOutcome::Failed { .. }));
    }

    #[test]
    fn no_terminal_event_is_an_error() {
        let stdout =
            "{\"event\":\"corpus_record_start\",\"id\":\"a\",\"text\":\"t\",\"seconds\":10}\n";
        let err = parse_corpus_record_result(stdout).unwrap_err();
        assert!(err.contains("no recording result"), "{err}");
    }

    #[test]
    fn tolerates_log_noise_and_non_json_lines() {
        let stdout = "[cap] opening device\nplain log line\n\
            {\"event\":\"corpus_record_done\",\"id\":\"z\",\"path\":\"z.wav\",\"seconds_recorded\":3.0}\n\
            trailing\n";
        let outcome = parse_corpus_record_result(stdout).unwrap();
        assert!(matches!(outcome, CorpusRecordOutcome::Saved { .. }));
    }

    #[test]
    fn done_without_peak_is_handled() {
        let stdout =
            "{\"event\":\"corpus_record_done\",\"id\":\"a\",\"path\":\"a.wav\",\"seconds_recorded\":2.0}\n";
        let outcome = parse_corpus_record_result(stdout).unwrap();
        match outcome {
            CorpusRecordOutcome::Saved { peak_dbfs, .. } => assert!(peak_dbfs.is_none()),
            other => panic!("expected Saved, got {other:?}"),
        }
    }

    #[test]
    fn log_detail_summarizes_saved_and_failed() {
        let saved = CorpusRecordOutcome::Saved {
            id: "da-001".to_owned(),
            path: "/a/da-001.wav".to_owned(),
            seconds_recorded: 9.8,
            peak_dbfs: Some(-6.0),
        };
        let detail = corpus_record_log_detail(&saved);
        assert!(detail.contains("da-001"));
        assert!(detail.contains("9.8s"));
        assert!(detail.contains("-6.0 dBFS"));

        let failed = CorpusRecordOutcome::Failed {
            error: "boom".to_owned(),
        };
        assert_eq!(corpus_record_log_detail(&failed), "failed: boom");
    }

    #[test]
    fn strings_present_in_en_and_da() {
        for key in [
            CorpusRecordText::SectionLabel,
            CorpusRecordText::PickerLabel,
            CorpusRecordText::RecordButton,
            CorpusRecordText::ReadAloudPrompt,
            CorpusRecordText::StopRuntimeHint,
            CorpusRecordText::Recording,
            CorpusRecordText::Saved,
            CorpusRecordText::RecordedMarker,
        ] {
            let en = corpus_record_text("en", key);
            let da = corpus_record_text("da", key);
            assert!(!en.is_empty(), "EN empty for {key:?}");
            assert!(!da.is_empty(), "DA empty for {key:?}");
            assert_ne!(en, da, "EN and DA must differ for {key:?}");
        }
    }

    #[test]
    fn record_button_label_is_localized() {
        assert_eq!(
            corpus_record_text("en", CorpusRecordText::RecordButton),
            "Record"
        );
        assert_eq!(
            corpus_record_text("da", CorpusRecordText::RecordButton),
            "Optag"
        );
    }
}
