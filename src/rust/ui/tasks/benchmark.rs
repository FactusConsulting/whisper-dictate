//! Native benchmark job and result presentation.

use super::*;

fn benchmark_summary_line(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("[benchmark]"))
}

impl WhisperDictateApp {
    /// Run the golden benchmark corpus off-thread using the native Rust
    /// runner ([`crate::benchmark::native::run_to_writer`]) — the same code
    /// path the `whisper-dictate bench` CLI verb drives: it runs
    /// entirely in-process on the shipping build (feature
    /// combo `whisper-rs-local,audio-capture`), and reports a clear rebuild
    /// hint on stock dev builds.
    ///
    /// The runner's output (per-item JSONL + the final `[benchmark] …`
    /// summary line) is captured on the background thread and handed to
    /// [`apply_benchmark_results`] via a synthesised [`BackgroundTaskResult`]
    /// so the digestible view + runtime-log lines stay identical to the
    /// previous shell-out path — the parser stays authoritative.
    ///
    /// Prints an immediate "benchmark started" line (only when the run
    /// actually starts, i.e. no other task is in flight) so the button
    /// never feels dead: the model load + corpus pass is slow, and without
    /// this the runtime log would stay silent for many seconds after the
    /// click.
    pub(in crate::ui) fn run_benchmark(&mut self) {
        if self.runtime_state != crate::runtime::RuntimeState::Stopped {
            self.append_runtime_log(format!(
                "[ui] {RUN_BENCHMARK_LABEL} skipped: stop the dictation runtime first"
            ));
            return;
        }
        if self.background_task.is_some() {
            self.append_runtime_log(format!(
                "[ui] {RUN_BENCHMARK_LABEL} skipped: another task is running"
            ));
            return;
        }
        // Clear any previous parsed results so the digestible view shows the
        // in-flight state, not a stale table from the last run. Only when
        // the run actually starts (no other task in flight) — mirrors the
        // start line so a gated click leaves the prior results visible.
        self.benchmark_results = None;
        self.append_runtime_log("[ui] benchmark started — results appear here when finished");
        // Log the equivalent of `command.display()` so the runtime log line
        // reads the same shape as every other background task — makes
        // native-vs-shellout indistinguishable in the log.
        let display = "bench (native)".to_owned();
        self.append_runtime_log(format!("[ui] {RUN_BENCHMARK_LABEL}: {display}"));
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf: Vec<u8> = Vec::new();
            let outcome = crate::benchmark::native::run_to_writer(&mut buf);
            let stdout = String::from_utf8_lossy(&buf).into_owned();
            let (success, code, error) = match outcome {
                Ok(()) => (true, Some(0), None),
                Err(crate::benchmark::native::NativeBenchError::Unsupported(reason)) => (
                    false,
                    Some(1),
                    Some(format!(
                        "`wd bench` is only available in the shipping build \
                         ({reason}); rebuild with --features whisper-rs-local,audio-capture"
                    )),
                ),
                Err(crate::benchmark::native::NativeBenchError::Other(e)) => {
                    (false, Some(1), Some(format!("{e:#}")))
                }
            };
            let _ = tx.send(BackgroundTaskResult {
                label: RUN_BENCHMARK_LABEL,
                command: display,
                stdout,
                stderr: String::new(),
                success,
                code,
                error,
            });
        });
        self.background_task = Some(rx);
        self.background_task_label = Some(RUN_BENCHMARK_LABEL);
    }

    /// Handle a finished benchmark run: parse the captured per-item JSONL
    /// stdout into the digestible [`BenchmarkResults`] model the System tab
    /// renders (a coloured headline + a worst-WER-first table), AND preserve the
    /// exact runtime-log behaviour the user already relied on — the per-item
    /// JSONL streamed verbatim plus the concise final `[benchmark] …` summary line
    /// (re-using `benchmark_summary_line` so a large blob is never re-embedded in
    /// one giant `[OK]` line). A run failure (worker couldn't even start) clears
    /// the model and logs the error, mirroring the generic failure path.
    pub(in crate::ui) fn apply_benchmark_results(&mut self, result: &BackgroundTaskResult) {
        // Stream the raw output to the log first, unchanged: the per-item JSONL
        // (and stderr) the user has always seen stays in the runtime log so the
        // digestible view is purely additive and the raw remains inspectable.
        self.append_runtime_output(result.stdout.trim_end());
        self.append_runtime_output(result.stderr.trim_end());

        if let Some(error) = &result.error {
            // The worker couldn't run at all — there is no stdout to parse. Clear
            // any stale model and surface the failure like the generic path did.
            self.benchmark_results = None;
            self.append_runtime_log(format!("[ERROR] {} failed to run: {error}", result.label));
            return;
        }

        // Parse the captured stdout into the model regardless of exit code — a
        // non-zero exit can still carry usable per-item rows worth showing.
        let results = parse_benchmark_results(&result.stdout);

        if result.success {
            // Preserve the original `[OK] … passed: [benchmark] …` line: carry only
            // the concise summary line, never the whole JSONL blob.
            let detail = benchmark_summary_line(&result.stdout).unwrap_or("");
            let message = if detail.is_empty() {
                format!("[OK] {} passed", result.label)
            } else {
                format!("[OK] {} passed: {detail}", result.label)
            };
            self.append_runtime_log(message);
        } else {
            let mut message = format!(
                "[ERROR] {} failed with code {}",
                result.label,
                result
                    .code
                    .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
            );
            if let Some(summary) = benchmark_summary_line(&result.stdout) {
                message.push_str(": ");
                message.push_str(summary);
            }
            self.append_runtime_log(message);
        }

        // Log the digestible one-line headline (the localized view lives in the
        // System tab) so even the log reader gets the at-a-glance result.
        if !results.is_empty() {
            self.append_runtime_log(format!(
                "[ui] benchmark: {}",
                benchmark_results_log_detail(&results)
            ));
        }
        self.benchmark_results = Some(results);
    }
}

#[cfg(test)]
mod benchmark_tests {
    use super::benchmark_summary_line;

    #[test]
    fn picks_the_last_benchmark_summary_line_and_ignores_jsonl() {
        let stdout = "\
{\"item\":1,\"wer\":0.1}
{\"item\":2,\"wer\":0.2}
[benchmark] 2/2 passed, avg WER 15.0%, avg CER 7.5%
";
        assert_eq!(
            benchmark_summary_line(stdout),
            Some("[benchmark] 2/2 passed, avg WER 15.0%, avg CER 7.5%"),
        );
    }

    #[test]
    fn returns_none_when_no_summary_line_present() {
        assert_eq!(benchmark_summary_line("{\"item\":1}\n"), None);
        assert_eq!(benchmark_summary_line(""), None);
    }
}
