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
    /// Intro line making the feature's PURPOSE unmistakable: these recordings are
    /// the benchmark's golden reference audio, not dictionary entries.
    PurposeIntro,
    /// One-liner near the Record action clarifying the overwrite/no-dictionary
    /// semantics of re-recording an item.
    OverwriteNote,
    /// Label for the "record every item that has no recording yet" batch button.
    RecordAllMissing,
    /// Label for the "re-record every item" batch button.
    RecordAll,
    /// Label for the batch Stop/cancel button.
    StopBatch,
    /// Label preceding the read-aloud reference text during a batch run (the
    /// "k of N" position is formatted by [`batch_progress_line`]).
    BatchReadAloudPrompt,
    /// Hint shown when a batch run found nothing to record (all items done).
    BatchNothingToRecord,
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
            Self::SectionLabel => "Benchmark reference recordings",
            Self::SectionHelp => {
                "Record the golden reference audio used by the speech-accuracy benchmark \
                 (System → Run benchmark): pick an item, read the shown text aloud after \
                 clicking Record, and the clip is saved where the benchmark scores it. This \
                 does NOT change your dictionary. Stop the dictation runtime first."
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
            Self::PurposeIntro => {
                "These recordings are the golden reference audio for the speech-accuracy \
                 benchmark (System → Run benchmark). They do NOT change your dictionary."
            }
            Self::OverwriteNote => {
                "Re-recording an item overwrites only that item's own audio file; your \
                 dictionary is never touched."
            }
            Self::RecordAllMissing => "Record all missing",
            Self::RecordAll => "Re-record all",
            Self::StopBatch => "Stop",
            Self::BatchReadAloudPrompt => "Read this aloud:",
            Self::BatchNothingToRecord => "Every corpus item already has a recording.",
        }
    }

    fn danish(self) -> &'static str {
        match self {
            Self::SectionLabel => "Benchmark-referenceoptagelser",
            Self::SectionHelp => {
                "Optag den gyldne reference-lyd som tale-nøjagtigheds-benchmarken bruger \
                 (System → Kør benchmark): vælg et element, læs den viste tekst højt efter \
                 du har klikket Optag, og klippet gemmes der hvor benchmarken scorer det. \
                 Dette ændrer IKKE din ordbog. Stop dikterings-runtimen først."
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
            Self::PurposeIntro => {
                "Disse optagelser er den gyldne reference-lyd til tale-nøjagtigheds-benchmarken \
                 (System → Kør benchmark). De ændrer IKKE din ordbog."
            }
            Self::OverwriteNote => {
                "At genoptage et element overskriver kun dette elements egen lydfil; din \
                 ordbog røres aldrig."
            }
            Self::RecordAllMissing => "Optag alle manglende",
            Self::RecordAll => "Genoptag alle",
            Self::StopBatch => "Stop",
            Self::BatchReadAloudPrompt => "Læs dette højt:",
            Self::BatchNothingToRecord => "Alle korpuselementer har allerede en optagelse.",
        }
    }
}

/// Resolve a corpus-record string for the active UI language. Mirrors the raw
/// `"da"` → Danish mapping used everywhere else (any other value is English),
/// kept local so this module stays self-contained for clean parallel merges.
pub(in crate::ui) fn corpus_record_text(raw_language: &str, key: CorpusRecordText) -> &'static str {
    key.label(raw_language == "da")
}

/// The localized batch progress line: "Item k of N · recorded c" (EN) /
/// "Element k af N · optaget c" (DA), where `position` is the 1-based current
/// item, `total` the run length, and `completed` how many clips already saved.
///
/// Kept here (next to the strings) and pure so it unit-tests without egui and the
/// EN/DA wording stays in one place. The reference text to read is rendered
/// separately by the caller — this is just the counter line.
pub(in crate::ui) fn batch_progress_line(
    raw_language: &str,
    position: usize,
    total: usize,
    completed: usize,
) -> String {
    if raw_language == "da" {
        format!("Element {position} af {total} · optaget {completed}")
    } else {
        format!("Item {position} of {total} · recorded {completed}")
    }
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
        // Keys where the EN and DA translations are intentionally identical
        // (e.g. "Stop" is the same word in both languages).
        let allow_identical = [CorpusRecordText::StopBatch];

        for key in [
            CorpusRecordText::SectionLabel,
            CorpusRecordText::SectionHelp,
            CorpusRecordText::PickerLabel,
            CorpusRecordText::RecordButton,
            CorpusRecordText::RecordButtonHelp,
            CorpusRecordText::ReadAloudPrompt,
            CorpusRecordText::StopRuntimeHint,
            CorpusRecordText::Recording,
            CorpusRecordText::NoItems,
            CorpusRecordText::Saved,
            CorpusRecordText::RecordedMarker,
            CorpusRecordText::PurposeIntro,
            CorpusRecordText::OverwriteNote,
            CorpusRecordText::RecordAllMissing,
            CorpusRecordText::RecordAll,
            CorpusRecordText::StopBatch,
            CorpusRecordText::BatchReadAloudPrompt,
            CorpusRecordText::BatchNothingToRecord,
        ] {
            let en = corpus_record_text("en", key);
            let da = corpus_record_text("da", key);
            assert!(!en.is_empty(), "EN empty for {key:?}");
            assert!(!da.is_empty(), "DA empty for {key:?}");
            if !allow_identical.contains(&key) {
                assert_ne!(en, da, "EN and DA must differ for {key:?}");
            }
        }
    }

    #[test]
    fn clarity_strings_name_the_benchmark_and_clear_the_dictionary() {
        // The whole point of the clarity work: the heading + intro must read as
        // benchmark reference audio, and the intro/overwrite note must say the
        // dictionary is untouched — in BOTH languages.
        let heading_en = corpus_record_text("en", CorpusRecordText::SectionLabel);
        assert!(
            heading_en.to_lowercase().contains("benchmark"),
            "EN heading should mention the benchmark: {heading_en}"
        );
        let heading_da = corpus_record_text("da", CorpusRecordText::SectionLabel);
        assert!(
            heading_da.to_lowercase().contains("benchmark"),
            "DA heading should mention the benchmark: {heading_da}"
        );

        let intro_en = corpus_record_text("en", CorpusRecordText::PurposeIntro);
        assert!(intro_en.to_lowercase().contains("benchmark"), "{intro_en}");
        assert!(intro_en.to_lowercase().contains("dictionary"), "{intro_en}");
        let intro_da = corpus_record_text("da", CorpusRecordText::PurposeIntro);
        assert!(intro_da.to_lowercase().contains("benchmark"), "{intro_da}");
        assert!(intro_da.to_lowercase().contains("ordbog"), "{intro_da}");

        let note_en = corpus_record_text("en", CorpusRecordText::OverwriteNote);
        assert!(
            note_en.to_lowercase().contains("overwrites"),
            "EN overwrite note should say it overwrites: {note_en}"
        );
        assert!(note_en.to_lowercase().contains("dictionary"), "{note_en}");
        let note_da = corpus_record_text("da", CorpusRecordText::OverwriteNote);
        assert!(note_da.to_lowercase().contains("overskriver"), "{note_da}");
        assert!(note_da.to_lowercase().contains("ordbog"), "{note_da}");
    }

    #[test]
    fn batch_progress_line_formats_k_of_n_per_language() {
        let en = batch_progress_line("en", 2, 5, 1);
        assert!(en.contains("Item 2 of 5"), "{en}");
        assert!(en.contains("recorded 1"), "{en}");

        let da = batch_progress_line("da", 2, 5, 1);
        assert!(da.contains("Element 2 af 5"), "{da}");
        assert!(da.contains("optaget 1"), "{da}");

        assert_ne!(en, da, "EN and DA progress lines must differ");
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
