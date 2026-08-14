//! Incremental, bounded projections of the runtime log used by the immediate-
//! mode UI. Appends do the parsing; repaints only borrow cached output.

use super::app::TRIM_MARKER;
use super::*;
use std::collections::VecDeque;

/// A separate ceiling prevents a stream of tiny status lines from retaining an
/// impractical number of card models even while the raw byte cap is respected.
pub(in crate::ui) const RUNTIME_LOG_MAX_CARDS: usize = 4_096;
const EDGE_FINGERPRINT_BYTES: usize = 64;
const NO_DATA: &str = "No data yet";

#[derive(Debug)]
pub(in crate::ui) struct RuntimeLogCache {
    entries: VecDeque<RuntimeLogLineProjection>,
    minimal_text: String,
    diagnostic_text: String,
    debug_text: String,
    debug_line_count: usize,
    minimal_cards: Vec<RuntimeLogCard>,
    diagnostic_cards: Vec<RuntimeLogCard>,
    has_structured_utterance: bool,
    has_structured_text: bool,
    latest_capture: Option<String>,
    latest_gate: Option<String>,
    latest_stt: Option<String>,
    latest_injection: Option<String>,
    latest_post_detail: Option<String>,
    source_len: usize,
    source_fingerprint: u64,
    revision: u64,
    parsed_line_count: u64,
    parse_failure_count: u64,
}

impl Default for RuntimeLogCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            minimal_text: String::new(),
            diagnostic_text: String::new(),
            debug_text: String::new(),
            debug_line_count: 0,
            minimal_cards: Vec::new(),
            diagnostic_cards: Vec::new(),
            has_structured_utterance: false,
            has_structured_text: false,
            latest_capture: None,
            latest_gate: None,
            latest_stt: None,
            latest_injection: None,
            latest_post_detail: None,
            source_len: 0,
            source_fingerprint: bounded_fingerprint(""),
            revision: 0,
            parsed_line_count: 0,
            parse_failure_count: 0,
        }
    }
}

impl RuntimeLogCache {
    pub(in crate::ui) fn from_log(log: &str) -> Self {
        let mut cache = Self::default();
        cache.reset_from_log(log);
        cache
    }

    /// Reconcile tests or future call sites that replace the public app field
    /// directly. The common append/render path only compares a fixed-size edge
    /// fingerprint and therefore never scans retained history on a quiet frame.
    pub(in crate::ui) fn sync_if_needed(&mut self, log: &str) {
        if self.source_len != log.len() || self.source_fingerprint != bounded_fingerprint(log) {
            self.reset_from_log(log);
        }
    }

    pub(in crate::ui) fn append(&mut self, appended: &str) {
        let had_structured_utterance = self.has_structured_utterance;
        let had_structured_text = self.has_structured_text;
        let start = self.entries.len();
        // `split` retains trailing and interior empty lines. A lone empty append
        // only represents a line when a prior raw line exists; appending it to
        // an empty raw String is still a no-op in `append_runtime_log`.
        if !appended.is_empty() || self.source_len > 0 {
            for line in appended.split('\n') {
                let projection = self.parse_line(line);
                self.has_structured_utterance |= projection.structured_utterance;
                self.has_structured_text |= projection.structured_text.is_some();
                self.update_latest(&projection);
                self.entries.push_back(projection);
            }
        }

        if (!had_structured_utterance && self.has_structured_utterance)
            || (!had_structured_text && self.has_structured_text)
        {
            // A structured utterance supersedes earlier truncated inject
            // previews. Rebuild from parsed projections, not raw JSON.
            self.rebuild_views();
        } else {
            let mut views = RuntimeLogViewBuffers {
                minimal_text: &mut self.minimal_text,
                diagnostic_text: &mut self.diagnostic_text,
                debug_text: &mut self.debug_text,
                debug_line_count: &mut self.debug_line_count,
                minimal_cards: &mut self.minimal_cards,
                diagnostic_cards: &mut self.diagnostic_cards,
            };
            for entry in self.entries.iter().skip(start) {
                append_entry_views(
                    entry,
                    self.has_structured_utterance,
                    self.has_structured_text,
                    &mut views,
                );
            }
            trim_cards(&mut self.minimal_cards);
            trim_cards(&mut self.diagnostic_cards);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Complete an append after the raw string has applied its established
    /// whole-line trimming semantics.
    pub(in crate::ui) fn finish_append(&mut self, log: &str, trimmed: bool) {
        if trimmed {
            self.retain_trimmed_tail(log);
        }
        self.set_source(log);
    }

    pub(in crate::ui) fn clear(&mut self) {
        self.entries.clear();
        self.clear_views_and_summaries();
        self.has_structured_utterance = false;
        self.has_structured_text = false;
        self.revision = self.revision.wrapping_add(1);
        self.set_source("");
    }

    pub(in crate::ui) fn text(&self, mode: LogViewMode) -> &str {
        match mode {
            LogViewMode::Minimal => &self.minimal_text,
            LogViewMode::Diagnostic => &self.diagnostic_text,
            LogViewMode::Debug => {
                if self
                    .entries
                    .back()
                    .is_some_and(|entry| entry.raw.is_empty())
                {
                    self.debug_text
                        .strip_suffix('\n')
                        .unwrap_or(&self.debug_text)
                } else {
                    &self.debug_text
                }
            }
        }
    }

    pub(in crate::ui) fn cards(&self, mode: LogViewMode) -> &[RuntimeLogCard] {
        match mode {
            LogViewMode::Minimal => &self.minimal_cards,
            LogViewMode::Diagnostic => &self.diagnostic_cards,
            LogViewMode::Debug => &[],
        }
    }

    pub(in crate::ui) fn latest_capture(&self) -> &str {
        self.latest_capture.as_deref().unwrap_or(NO_DATA)
    }

    pub(in crate::ui) fn latest_gate(&self) -> &str {
        self.latest_gate.as_deref().unwrap_or(NO_DATA)
    }

    pub(in crate::ui) fn latest_stt(&self) -> &str {
        self.latest_stt.as_deref().unwrap_or(NO_DATA)
    }

    pub(in crate::ui) fn latest_injection(&self) -> &str {
        self.latest_injection.as_deref().unwrap_or(NO_DATA)
    }

    #[cfg(test)]
    pub(in crate::ui) fn stats(&self) -> (u64, u64, u64) {
        (
            self.revision,
            self.parsed_line_count,
            self.parse_failure_count,
        )
    }

    #[cfg(test)]
    pub(in crate::ui) fn view_key(&self, mode: LogViewMode) -> (u64, LogViewMode) {
        (self.revision, mode)
    }

    fn reset_from_log(&mut self, log: &str) {
        self.entries.clear();
        self.clear_views_and_summaries();
        self.has_structured_utterance = false;
        self.has_structured_text = false;
        if !log.is_empty() {
            for line in log.split('\n') {
                let projection = self.parse_line(line);
                self.has_structured_utterance |= projection.structured_utterance;
                self.has_structured_text |= projection.structured_text.is_some();
                self.update_latest(&projection);
                self.entries.push_back(projection);
            }
        }
        self.rebuild_views();
        self.revision = self.revision.wrapping_add(1);
        self.set_source(log);
    }

    fn parse_line(&mut self, line: &str) -> RuntimeLogLineProjection {
        let projection = project_runtime_log_line(line, self.latest_post_detail.as_deref());
        self.parsed_line_count = self.parsed_line_count.saturating_add(1);
        if projection.utterance_parse_failed {
            self.parse_failure_count = self.parse_failure_count.saturating_add(1);
        }
        projection
    }

    fn retain_trimmed_tail(&mut self, log: &str) {
        let body = log
            .strip_prefix(TRIM_MARKER)
            .and_then(|tail| tail.strip_prefix('\n'))
            .unwrap_or("");
        let retained_lines = if body.is_empty() {
            0
        } else {
            body.split('\n').count()
        };
        self.entries.retain(|entry| entry.raw != TRIM_MARKER);
        while self.entries.len() > retained_lines {
            self.entries.pop_front();
        }
        let marker = self.parse_line(TRIM_MARKER);
        self.entries.push_front(marker);
        self.has_structured_utterance = self.entries.iter().any(|entry| entry.structured_utterance);
        self.has_structured_text = self
            .entries
            .iter()
            .any(|entry| entry.structured_text.is_some());
        self.refresh_retained_post_context();
        self.rebuild_views();
    }

    fn refresh_retained_post_context(&mut self) {
        let mut previous_post: Option<String> = None;
        for entry in &mut self.entries {
            if let Some(text) = &entry.inject_text {
                entry.diagnostic_without_utterances = Some(RuntimeLogCard {
                    kind: RuntimeLogCardKind::FinalText,
                    title: text.clone(),
                    detail: previous_post
                        .clone()
                        .unwrap_or_else(|| "Final output".to_owned()),
                    badge: "Final".to_owned(),
                });
            }
            if entry.raw.starts_with("[post]") {
                previous_post = Some(strip_log_prefix(&entry.raw).to_owned());
            }
        }
    }

    fn rebuild_views(&mut self) {
        self.minimal_text.clear();
        self.diagnostic_text.clear();
        self.debug_text.clear();
        self.debug_line_count = 0;
        self.minimal_cards.clear();
        self.diagnostic_cards.clear();
        self.latest_capture = None;
        self.latest_gate = None;
        self.latest_stt = None;
        self.latest_injection = None;
        self.latest_post_detail = None;
        let mut views = RuntimeLogViewBuffers {
            minimal_text: &mut self.minimal_text,
            diagnostic_text: &mut self.diagnostic_text,
            debug_text: &mut self.debug_text,
            debug_line_count: &mut self.debug_line_count,
            minimal_cards: &mut self.minimal_cards,
            diagnostic_cards: &mut self.diagnostic_cards,
        };
        for entry in &self.entries {
            append_entry_views(
                entry,
                self.has_structured_utterance,
                self.has_structured_text,
                &mut views,
            );
            update_latest_fields(
                entry,
                &mut self.latest_capture,
                &mut self.latest_gate,
                &mut self.latest_stt,
                &mut self.latest_injection,
                &mut self.latest_post_detail,
            );
        }
        trim_cards(&mut self.minimal_cards);
        trim_cards(&mut self.diagnostic_cards);
    }

    fn update_latest(&mut self, entry: &RuntimeLogLineProjection) {
        update_latest_fields(
            entry,
            &mut self.latest_capture,
            &mut self.latest_gate,
            &mut self.latest_stt,
            &mut self.latest_injection,
            &mut self.latest_post_detail,
        );
    }

    fn clear_views_and_summaries(&mut self) {
        self.minimal_text.clear();
        self.diagnostic_text.clear();
        self.debug_text.clear();
        self.debug_line_count = 0;
        self.minimal_cards.clear();
        self.diagnostic_cards.clear();
        self.latest_capture = None;
        self.latest_gate = None;
        self.latest_stt = None;
        self.latest_injection = None;
        self.latest_post_detail = None;
    }

    fn set_source(&mut self, log: &str) {
        self.source_len = log.len();
        self.source_fingerprint = bounded_fingerprint(log);
    }
}

struct RuntimeLogViewBuffers<'a> {
    minimal_text: &'a mut String,
    diagnostic_text: &'a mut String,
    debug_text: &'a mut String,
    debug_line_count: &'a mut usize,
    minimal_cards: &'a mut Vec<RuntimeLogCard>,
    diagnostic_cards: &'a mut Vec<RuntimeLogCard>,
}

fn append_entry_views(
    entry: &RuntimeLogLineProjection,
    has_structured_utterance: bool,
    has_structured_text: bool,
    views: &mut RuntimeLogViewBuffers<'_>,
) {
    append_debug_line(views.debug_text, views.debug_line_count, &entry.debug);
    if entry.diagnostic {
        append_line(views.diagnostic_text, &entry.raw);
    }
    let output = if has_structured_text {
        entry.structured_text.as_deref()
    } else {
        entry.inject_text.as_deref()
    };
    if let Some(output) = output {
        append_line(views.minimal_text, output);
    }
    let minimal = if has_structured_utterance {
        &entry.minimal_with_utterances
    } else {
        &entry.minimal_without_utterances
    };
    if let Some(card) = minimal {
        views.minimal_cards.push(card.clone());
    }
    let diagnostic = if has_structured_utterance {
        &entry.diagnostic_with_utterances
    } else {
        &entry.diagnostic_without_utterances
    };
    if let Some(card) = diagnostic {
        views.diagnostic_cards.push(card.clone());
    }
}

fn append_debug_line(target: &mut String, line_count: &mut usize, line: &str) {
    if *line_count > 0 {
        target.push('\n');
    }
    target.push_str(line);
    *line_count = line_count.saturating_add(1);
}

fn append_line(target: &mut String, line: &str) {
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(line);
}

fn trim_cards(cards: &mut Vec<RuntimeLogCard>) {
    if cards.len() > RUNTIME_LOG_MAX_CARDS {
        cards.drain(..cards.len() - RUNTIME_LOG_MAX_CARDS);
    }
}

fn update_latest_fields(
    entry: &RuntimeLogLineProjection,
    capture: &mut Option<String>,
    gate: &mut Option<String>,
    stt: &mut Option<String>,
    injection: &mut Option<String>,
    post: &mut Option<String>,
) {
    if entry.raw.starts_with("[cap]") {
        *capture = Some(compact_diagnostic_title(&entry.raw));
    }
    if entry.raw.starts_with("[gate]") {
        *gate = Some(compact_diagnostic_title(&entry.raw));
    }
    if entry.raw.starts_with("[stt]") {
        *stt = Some(compact_diagnostic_title(&entry.raw));
    }
    if entry.raw.starts_with("[inject] strategy:") {
        *injection = Some(strip_log_prefix(&entry.raw).to_owned());
    }
    if entry.raw.starts_with("[post]") {
        *post = Some(strip_log_prefix(&entry.raw).to_owned());
    }
}

fn bounded_fingerprint(log: &str) -> u64 {
    let bytes = log.as_bytes();
    let mut hash = 0xcbf29ce484222325_u64 ^ bytes.len() as u64;
    for byte in bytes
        .iter()
        .take(EDGE_FINGERPRINT_BYTES)
        .chain(bytes.iter().rev().take(EDGE_FINGERPRINT_BYTES))
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
