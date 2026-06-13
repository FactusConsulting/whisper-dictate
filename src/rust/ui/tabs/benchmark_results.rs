//! The System tab's "Benchmark results" view (rendering only).
//!
//! Split out of `tabs/system.rs` so that file stays under the module-size limit
//! and so the benchmark-results feature's UI lives in one place. The pure parser,
//! display model and localized strings live in `ui/benchmark_results.rs`; this
//! module only paints them.
//!
//! It turns the parsed [`BenchmarkResults`] into the digestible view the user
//! asked for: a single coloured HEADLINE line ("12/31 scored · avg WER 8.4% ·
//! avg CER 3.1% · 19 skipped") plus a compact, worst-WER-first TABLE of items
//! (Item | Lang | WER% | CER% | Status) in a scroll area, with each row coloured
//! by outcome (green low WER, amber mid, red high/error, grey skipped). The raw
//! per-item JSONL stays in the runtime log and is also revealable here behind a
//! "Show raw output" toggle — the digestible view is the point, the raw is a
//! fallback.

use super::super::*;
use super::*;

/// WER thresholds (as 0..1 fractions) for the green/amber/red row + headline
/// colouring. Below `WER_GOOD` is green ("pass-ish"), up to `WER_OK` is amber,
/// above is red. Tuned for dictation: ~10% WER is a clean run, ~25%+ is poor.
const WER_GOOD: f32 = 0.10;
const WER_OK: f32 = 0.25;

impl WhisperDictateApp {
    /// Render the digestible benchmark-results view below the Run-benchmark
    /// button: a spinner while a run is in flight, then a coloured headline + a
    /// worst-WER-first table once results are parsed. Reads `benchmark_results`
    /// (transient UI state); renders nothing extra before the first run.
    pub(in crate::ui) fn benchmark_results_section(
        &mut self,
        ui: &mut egui::Ui,
        palette: UiPalette,
    ) {
        let language = self.settings.ui_language.clone();
        let running = self.background_task_label == Some(crate::ui::tasks::RUN_BENCHMARK_LABEL);

        // Nothing to show before the first run and while no run is in flight: keep
        // the maintenance cluster uncluttered until there is a result.
        if !running && self.benchmark_results.is_none() {
            return;
        }

        ui.add_space(10.0);
        section_label(
            ui,
            benchmark_text(&language, BenchmarkText::SectionLabel),
            palette,
        );
        ui.add_space(6.0);

        if running {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    egui::RichText::new(ui_text(&language, UiTextKey::RunBenchmarkHelp))
                        .color(palette.text_muted),
                );
            });
            // While running we still show the PREVIOUS results below (if any) so
            // the panel doesn't flash empty — fall through to render them.
        }

        let Some(results) = self.benchmark_results.as_ref() else {
            return;
        };

        if results.is_empty() {
            // Parsing produced no per-item rows (e.g. only the trailing
            // `[benchmark] …` summary or error text is present). Show a brief
            // note so the panel is not confusingly blank, but still expose the
            // raw output — it is the only thing the user can inspect.
            ui.label(
                egui::RichText::new(benchmark_text(&language, BenchmarkText::NoRowsParsed))
                    .color(palette.text_muted),
            );
        } else {
            headline(ui, &language, palette, &results.summary);
            ui.add_space(8.0);
            results_table(ui, &language, palette, &results.rows);
        }

        // Raw output stays in the runtime log; it is also revealable here as a
        // fallback for the user who wants the original JSONL without leaving the
        // tab. Collapsed by default — the digestible view is the point.
        // Always shown when raw is non-empty so it is inspectable even when no
        // per-item rows were parsed (e.g. the run only produced the summary line
        // or an error message).
        if should_show_raw(results) {
            ui.add_space(6.0);
            egui::CollapsingHeader::new(benchmark_text(&language, BenchmarkText::ShowRaw))
                .id_salt("benchmark_raw_output")
                .default_open(false)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(results.raw.as_str()).monospace())
                            .selectable(true),
                    );
                });
        }
    }
}

/// Paint the coloured one-line headline summarising the run. Green when every
/// item scored and the average WER is good; amber when there are skips/errors or
/// a middling average WER; red when the average WER is poor. The metrics are
/// shown as percentages (wer*100, 1 decimal), never raw 0..1 floats.
fn headline(ui: &mut egui::Ui, language: &str, palette: UiPalette, summary: &BenchmarkSummary) {
    let mut parts = vec![format!(
        "{}/{} {}",
        summary.scored,
        summary.total,
        benchmark_text(language, BenchmarkText::Scored)
    )];
    if let Some(avg) = summary.avg_wer {
        parts.push(format!(
            "{} {}",
            benchmark_text(language, BenchmarkText::AvgWer),
            format_rate_percent(Some(avg))
        ));
    }
    if let Some(avg) = summary.avg_cer {
        parts.push(format!(
            "{} {}",
            benchmark_text(language, BenchmarkText::AvgCer),
            format_rate_percent(Some(avg))
        ));
    }
    if summary.skipped > 0 {
        parts.push(format!(
            "{} {}",
            summary.skipped,
            benchmark_text(language, BenchmarkText::Skipped)
        ));
    }
    if summary.error > 0 {
        parts.push(format!(
            "{} {}",
            summary.error,
            benchmark_text(language, BenchmarkText::Error)
        ));
    }
    let text = parts.join(" · ");
    let color = headline_color(palette, summary);
    ui.label(
        egui::RichText::new(text)
            .color(color)
            .text_style(egui::TextStyle::Body)
            .strong(),
    );
}

/// Choose the headline colour: red on a poor average WER, amber when there are
/// skips/errors or a middling average WER, green for an all-scored, low-WER run.
fn headline_color(palette: UiPalette, summary: &BenchmarkSummary) -> egui::Color32 {
    match summary.avg_wer {
        Some(avg) if avg > WER_OK => palette.error_text,
        Some(avg) if avg <= WER_GOOD && summary.skipped == 0 && summary.error == 0 => {
            palette.ok_text
        }
        // Scored but middling, or has skips/errors → amber.
        Some(_) => palette.warn_text,
        // Nothing scored at all (e.g. every item skipped for missing audio) →
        // amber: not a failure, but the user has nothing to evaluate yet.
        None => palette.warn_text,
    }
}

/// Paint the compact item table inside a bounded scroll area: one header row
/// (Item | Lang | WER% | CER% | Status) then the rows in worst-WER-first order,
/// each coloured by outcome. Skipped/error rows are de-emphasized (grey/amber).
fn results_table(ui: &mut egui::Ui, language: &str, palette: UiPalette, rows: &[BenchmarkRow]) {
    egui::ScrollArea::vertical()
        .max_height(280.0)
        .id_salt("benchmark_results_table")
        .show(ui, |ui| {
            egui::Grid::new("benchmark_results_grid")
                .num_columns(5)
                .striped(true)
                .spacing(egui::vec2(16.0, 6.0))
                .show(ui, |ui| {
                    header_row(ui, language, palette);
                    for row in rows {
                        item_row(ui, language, palette, row);
                    }
                });
        });
}

fn header_row(ui: &mut egui::Ui, language: &str, palette: UiPalette) {
    for key in [
        BenchmarkText::ColItem,
        BenchmarkText::ColLang,
        BenchmarkText::ColWer,
        BenchmarkText::ColCer,
        BenchmarkText::ColStatus,
    ] {
        ui.label(
            egui::RichText::new(benchmark_text(language, key))
                .text_style(egui::TextStyle::Small)
                .strong()
                .color(palette.text_muted),
        );
    }
    ui.end_row();
}

fn item_row(ui: &mut egui::Ui, language: &str, palette: UiPalette, row: &BenchmarkRow) {
    let color = row_color(palette, row);
    ui.label(egui::RichText::new(&row.id).color(color));
    ui.label(egui::RichText::new(&row.language).color(color));
    ui.label(egui::RichText::new(format_rate_percent(row.wer)).color(color));
    ui.label(egui::RichText::new(format_rate_percent(row.cer)).color(color));
    ui.label(egui::RichText::new(status_word(language, row.status)).color(color));
    ui.end_row();
}

/// The localized status word for a row's outcome.
fn status_word(language: &str, status: BenchmarkStatus) -> &'static str {
    match status {
        BenchmarkStatus::Scored => benchmark_text(language, BenchmarkText::StatusScored),
        BenchmarkStatus::Skipped => benchmark_text(language, BenchmarkText::Skipped),
        BenchmarkStatus::Error => benchmark_text(language, BenchmarkText::Error),
    }
}

/// The row colour: scored rows are green/amber/red by WER band; error rows are
/// red; skipped rows are de-emphasized grey (text_muted).
fn row_color(palette: UiPalette, row: &BenchmarkRow) -> egui::Color32 {
    match row.status {
        BenchmarkStatus::Skipped => palette.text_muted,
        BenchmarkStatus::Error => palette.error_text,
        BenchmarkStatus::Scored => match row.wer {
            Some(wer) if wer > WER_OK => palette.error_text,
            Some(wer) if wer <= WER_GOOD => palette.ok_text,
            _ => palette.warn_text,
        },
    }
}

/// Whether to render the "Show raw output" collapsible panel. True whenever the
/// captured stdout is non-empty — regardless of whether any per-item rows were
/// parsed — so the raw is always inspectable (even when parsing failed or only
/// a trailing `[benchmark] …` / error text was captured).
fn should_show_raw(results: &BenchmarkResults) -> bool {
    !results.raw.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> UiPalette {
        ui_palette("dark")
    }

    fn row(status: BenchmarkStatus, wer: Option<f32>) -> BenchmarkRow {
        BenchmarkRow {
            id: "x".to_owned(),
            language: "da".to_owned(),
            category: String::new(),
            status,
            wer,
            cer: None,
            exact_match: false,
            term_hits: 0,
            term_misses: 0,
            error: None,
        }
    }

    fn summary(
        scored: usize,
        skipped: usize,
        error: usize,
        avg_wer: Option<f32>,
    ) -> BenchmarkSummary {
        BenchmarkSummary {
            total: scored + skipped + error,
            scored,
            skipped,
            error,
            avg_wer,
            avg_cer: None,
        }
    }

    #[test]
    fn row_color_bands_scored_rows_green_amber_red() {
        let p = dark();
        assert_eq!(
            row_color(p, &row(BenchmarkStatus::Scored, Some(0.05))),
            p.ok_text
        );
        assert_eq!(
            row_color(p, &row(BenchmarkStatus::Scored, Some(0.18))),
            p.warn_text
        );
        assert_eq!(
            row_color(p, &row(BenchmarkStatus::Scored, Some(0.40))),
            p.error_text
        );
    }

    #[test]
    fn row_color_de_emphasizes_skipped_and_reds_error() {
        let p = dark();
        assert_eq!(
            row_color(p, &row(BenchmarkStatus::Skipped, None)),
            p.text_muted
        );
        assert_eq!(
            row_color(p, &row(BenchmarkStatus::Error, None)),
            p.error_text
        );
    }

    #[test]
    fn headline_is_green_only_when_all_scored_and_low_wer() {
        let p = dark();
        // All scored, low avg WER → green.
        assert_eq!(headline_color(p, &summary(10, 0, 0, Some(0.05))), p.ok_text);
        // Low WER but with skips → amber, not green.
        assert_eq!(
            headline_color(p, &summary(8, 2, 0, Some(0.05))),
            p.warn_text
        );
        // Middling WER → amber.
        assert_eq!(
            headline_color(p, &summary(10, 0, 0, Some(0.18))),
            p.warn_text
        );
        // Poor WER → red.
        assert_eq!(
            headline_color(p, &summary(10, 0, 0, Some(0.40))),
            p.error_text
        );
        // Nothing scored → amber.
        assert_eq!(headline_color(p, &summary(0, 5, 0, None)), p.warn_text);
    }

    #[test]
    fn status_word_is_localized() {
        assert_eq!(status_word("en", BenchmarkStatus::Scored), "Scored");
        assert_eq!(status_word("da", BenchmarkStatus::Scored), "Scoret");
        assert_eq!(status_word("en", BenchmarkStatus::Skipped), "skipped");
        assert_eq!(status_word("da", BenchmarkStatus::Skipped), "sprunget over");
        assert_eq!(status_word("en", BenchmarkStatus::Error), "error");
        assert_eq!(status_word("da", BenchmarkStatus::Error), "fejl");
    }

    fn empty_results(raw: &str) -> BenchmarkResults {
        BenchmarkResults {
            summary: BenchmarkSummary {
                total: 0,
                scored: 0,
                skipped: 0,
                error: 0,
                avg_wer: None,
                avg_cer: None,
            },
            rows: vec![],
            raw: raw.to_owned(),
        }
    }

    #[test]
    fn raw_panel_shown_even_when_no_rows_parsed() {
        // When parsing fails or only a summary/error line was captured, the raw
        // panel must still be offered so the user can inspect what happened.
        let with_noise = empty_results("[benchmark] 2/4 passed\n");
        assert!(
            should_show_raw(&with_noise),
            "raw panel must show when raw is non-empty but rows is empty"
        );

        // Completely empty stdout: nothing to show.
        let truly_empty = empty_results("");
        assert!(
            !should_show_raw(&truly_empty),
            "raw panel must not show when raw is empty"
        );
    }
}
