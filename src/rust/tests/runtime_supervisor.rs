use std::env;
#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(windows)]
use std::fs;
use std::path::PathBuf;
#[cfg(windows)]
use std::process::Child;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use whisper_dictate_app::runtime::cleanup_stale_desktop_processes;
use whisper_dictate_app::runtime::{RuntimeEvent, RuntimeState, RuntimeSupervisor, WorkerCommand};

#[test]
fn supervisor_captures_stdout_and_exit() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('worker-ready', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "worker-ready") && has_exit(events)
    });

    assert!(has_stdout(&events, "worker-ready"));
    assert!(has_exit(&events));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_fires_repaint_notifier_on_every_event() {
    // The whole point of the notifier — every runtime event published on the
    // channel must wake the consumer (egui). Without this the tray icon stays
    // GREEN through a full PTT cycle when the window has no foreground
    // attention. Drive it with a real child so the counter covers start() and
    // stream_lines (stdout); the stop()/wait thread is covered by the
    // separate test below.
    let Some(python) = test_python() else {
        return;
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_cb = Arc::clone(&count);
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.set_repaint_notifier(Arc::new(move || {
        count_for_cb.fetch_add(1, Ordering::SeqCst);
    }));
    assert!(supervisor.has_repaint_notifier());
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec!["-c".to_owned(), "print('hello', flush=True)".to_owned()],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    let _ = collect_until(&mut supervisor, |events| {
        has_stdout(events, "hello") && has_exit(events)
    });
    // Two channel-sends are guaranteed: Started (from start()) and one
    // Stdout (from the stream_lines thread). The Exited event is sent by
    // poll() on the main thread which deliberately does NOT notify — the
    // consumer is already there. So `>= 2` is the right bar.
    let total = count.load(Ordering::SeqCst);
    assert!(
        total >= 2,
        "repaint notifier should fire on every channel send; got {total}"
    );
}

#[test]
fn supervisor_fires_repaint_notifier_from_stop_thread() {
    // stop() spawns a thread that waits for the child to terminate and then
    // sends Exited (or Error on failure) through the channel and fires the
    // notifier. Cover that explicit path so coverage hits both branches in
    // the spawned closure.
    let Some(python) = test_python() else {
        return;
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_cb = Arc::clone(&count);
    let mut supervisor = RuntimeSupervisor::new();
    supervisor.set_repaint_notifier(Arc::new(move || {
        count_for_cb.fetch_add(1, Ordering::SeqCst);
    }));
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    let _ = collect_until(&mut supervisor, |events| has_stdout(events, "ready"));
    let before_stop = count.load(Ordering::SeqCst);
    supervisor.stop().unwrap();
    let _ = collect_until(&mut supervisor, has_exit);
    // stop()'s wait-thread is the only path that bumps the notifier between
    // before_stop and now — the Exited send + notifier() inside that closure.
    let after_stop = count.load(Ordering::SeqCst);
    assert!(
        after_stop > before_stop,
        "stop() wait-thread should fire notifier; before={before_stop} after={after_stop}"
    );
}

#[test]
fn supervisor_forces_utf8_stdio_for_piped_python_worker() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                concat!(
                    "import os; ",
                    "print(os.environ.get('PYTHONUTF8'), flush=True); ",
                    "print(os.environ.get('PYTHONIOENCODING'), flush=True); ",
                    "print('ændret prøv', flush=True)"
                )
                .to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "ændret prøv") && has_exit(events)
    });

    assert!(has_stdout(&events, "1"));
    assert!(has_stdout(&events, "utf-8"));
    assert!(has_stdout(&events, "ændret prøv"));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Stdout(line) if line.contains("Ã¦") || line.contains("Ã¸")
    )));
    assert!(has_exit(&events));
}

#[test]
fn supervisor_stops_running_process() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));
    assert_eq!(supervisor.state(), RuntimeState::Running);

    supervisor.stop().unwrap();
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
    let events = collect_until(&mut supervisor, has_exit);
    assert!(has_exit(&events));
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_stop_returns_without_waiting_for_process_exit_event() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));

    let started = Instant::now();
    supervisor.stop().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "stop should return immediately instead of waiting for process teardown"
    );
    assert_eq!(supervisor.state(), RuntimeState::Stopped);
}

#[test]
fn supervisor_restart_returns_without_waiting_for_process_exit_event() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python.clone(),
            args: vec![
                "-c".to_owned(),
                "import time; print('worker-ready', flush=True); time.sleep(30)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| has_stdout(events, "worker-ready"));
    assert!(has_stdout(&events, "worker-ready"));

    let started = Instant::now();
    supervisor
        .restart(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('worker-restarted', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "restart should return immediately instead of waiting for process teardown"
    );

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "worker-restarted")
    });
    assert!(has_stdout(&events, "worker-restarted"));
}

#[test]
fn supervisor_parses_worker_events_from_stderr() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import os, sys; assert os.environ['VOICEPI_WORKER_EVENTS'] == '1'; print('[worker-event] {\"event\":\"status\",\"state\":\"ready\"}', file=sys.stderr, flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_worker_status(events, "ready") && has_exit(events)
    });

    assert!(has_worker_status(&events, "ready"));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Stderr(line) if line.starts_with("[worker-event]")
    )));
}

#[test]
fn supervisor_passes_command_env_to_worker_without_logging_secret() {
    let Some(python) = test_python() else {
        return;
    };
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "import os; print(len(os.environ.get('VOICEPI_TEST_SECRET', '')) == 12, flush=True)"
                    .to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: vec![("VOICEPI_TEST_SECRET".to_owned(), "secret-value".to_owned())],
        })
        .unwrap();

    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "True") && has_exit(events)
    });

    assert!(has_stdout(&events, "True"));
    assert!(!events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Started { command } if command.contains("secret-value")
    )));
}

/// Iteration-2 review finding #6: the audio-in-rust path adds
/// `Stdio::piped()` on the worker's stdin and the supervisor writes
/// to it from the bridge thread. Windows named-pipe semantics differ
/// from Unix (chunked writes, handle inheritance via
/// `STARTUPINFO.hStdInput`, hidden child window via
/// `CREATE_NO_WINDOW`), so we pin the round-trip end-to-end on
/// Windows CI here. The test does NOT depend on the `audio-in-rust`
/// cargo feature: it mirrors what the supervisor does — spawn a
/// Python child with piped stdin, write bytes, verify the child read
/// them and printed them back on stdout — so a regression in the
/// supervisor's Windows pipe-handling shows up even in default
/// builds.
#[cfg(windows)]
#[test]
fn windows_child_reads_piped_stdin_written_by_parent() {
    use std::io::Write;
    use std::process::Stdio;
    let Some(python) = test_python() else {
        return;
    };
    // Mirror what `RuntimeSupervisor::start` does for the
    // audio-in-rust path: Stdio::piped() on stdin + the hidden-window
    // creation flag. We bypass the supervisor itself because it owns
    // stdin internally (the audio bridge takes it on spawn), so we
    // drive a bare Command here and just prove the pipe round-trips.
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let mut child = Command::new(python)
        .args([
            "-c",
            // The child reads a line, prints it back, and exits.
            // sys.stdin.readline blocks until the parent writes — if
            // the Windows pipe handle wasn't inherited correctly the
            // read would never return and the test would time out.
            "import sys; line = sys.stdin.readline(); print(f'got:{line.strip()}', flush=True)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn python with piped stdin on Windows");

    {
        let stdin = child.stdin.as_mut().expect("piped stdin is Some");
        stdin
            .write_all(b"hello-from-rust\n")
            .expect("Windows pipe write succeeds");
        stdin.flush().expect("Windows pipe flush succeeds");
    }
    // Dropping stdin closes the Windows pipe handle so the child sees
    // EOF after the line above — same teardown shape the
    // `BridgeHandle::stop` path uses for the real bridge.
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("child terminates after stdin EOF");
    assert!(
        output.status.success(),
        "child exited non-zero on Windows: {:?}",
        output.status,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("got:hello-from-rust"),
        "Windows piped child must echo the parent's stdin line; got: {stdout:?}",
    );
}

#[cfg(windows)]
#[test]
fn cleanup_stale_desktop_processes_stops_worker_from_same_app_root() {
    let Some(python) = test_python() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let package = dir
        .path()
        .join("src")
        .join("python")
        .join("whisper_dictate");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    let worker = package.join("runtime.py");
    fs::write(
        &worker,
        "import time\nprint('worker-ready', flush=True)\ntime.sleep(60)\n",
    )
    .unwrap();
    let root_arg = dir.path().display().to_string();
    let mut child = Command::new(python)
        .args(["-m", "whisper_dictate.runtime", "--app-root", &root_arg])
        .current_dir(dir.path())
        .env("PYTHONPATH", dir.path().join("src").join("python"))
        .spawn()
        .unwrap();
    let _app_root_guard = EnvVarGuard::set("VOICEPI_APP_ROOT", dir.path());

    cleanup_stale_desktop_processes();

    assert!(
        wait_for_exit(&mut child, Duration::from_secs(5)),
        "stale worker should be stopped by startup cleanup"
    );
}

// ---------------------------------------------------------------------------
// Audit item 5 Phase B step 1: `VOICEPI_DICTATE_ENGINE=rust` in-process
// dispatch. See `docs/design/item5-phase-b-inprocess.md`.
// ---------------------------------------------------------------------------
//
// The unit tests for the env-var gate + install-error taxonomy live next
// to the code under test at `src/rust/runtime/in_process.rs::tests`. This
// file carries the integration-level tests that DRIVE
// `RuntimeSupervisor::start` under the different engine choices.
//
// These tests set / clear `VOICEPI_DICTATE_ENGINE` inside a process-wide
// mutex (`ENGINE_ENV_LOCK`) because `std::env::set_var` is not
// thread-safe on Unix and the harness runs tests in parallel by default.
// The lock is scoped to the supervisor-facing tests only; unrelated
// tests in this file leave the env untouched.

use std::sync::{Mutex, MutexGuard, OnceLock};

fn engine_env_lock() -> MutexGuard<'static, ()> {
    static ENGINE_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENGINE_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

struct EngineEnvGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<String>,
}

impl EngineEnvGuard {
    fn set(value: &str) -> Self {
        let lock = engine_env_lock();
        let previous = env::var("VOICEPI_DICTATE_ENGINE").ok();
        env::set_var("VOICEPI_DICTATE_ENGINE", value);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for EngineEnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => env::set_var("VOICEPI_DICTATE_ENGINE", v),
            None => env::remove_var("VOICEPI_DICTATE_ENGINE"),
        }
    }
}

#[test]
fn supervisor_phase_b_stock_build_falls_back_to_python_on_engine_rust() {
    // Contract for stock builds (no rust-hotkeys+rust-injection): the
    // supervisor logs the actionable fallback message on stderr AND
    // spawns the Python worker anyway. Without this fallback, an
    // operator who set `=rust` on a stock build would see no PTT at
    // all — the design doc's risk #3 auto-fallback covers exactly
    // this class.
    #[cfg(not(all(feature = "rust-hotkeys", feature = "rust-injection")))]
    {
        let Some(python) = test_python() else {
            return;
        };
        let _guard = EngineEnvGuard::set("rust");
        let mut supervisor = RuntimeSupervisor::new();
        supervisor
            .start(WorkerCommand {
                program: python,
                args: vec![
                    "-c".to_owned(),
                    "print('python-fallback-ran', flush=True)".to_owned(),
                ],
                working_dir: env::current_dir().unwrap(),
                env: Vec::new(),
            })
            .unwrap();
        let events = collect_until(&mut supervisor, |events| {
            has_stdout(events, "python-fallback-ran") && has_exit(events)
        });
        // The Python fallback actually ran — the child produced our stdout.
        assert!(
            has_stdout(&events, "python-fallback-ran"),
            "Python fallback must run when Phase B refuses on stock build"
        );
        // AND the supervisor emitted the actionable fallback stderr line
        // naming why Phase B refused.
        let saw_fallback_stderr = events.iter().any(|e| {
            matches!(
                e,
                RuntimeEvent::Stderr(line)
                    if line.contains("Phase B in-process dispatch refused")
                        && line.contains("rust-hotkeys")
            )
        });
        assert!(
            saw_fallback_stderr,
            "supervisor must log the Phase B fallback reason on stock build; events: {events:#?}",
        );
    }
    // On a feature-complete build this test is a no-op because the
    // in-process install would actually run (which needs a display,
    // audio, and a live rdev listener — not something the CI harness
    // can drive). The feature-complete path is verified by
    // `supervisor_phase_b_engine_python_leaves_default_path_intact`
    // below plus the `in_process::tests` unit tests.
    #[cfg(all(feature = "rust-hotkeys", feature = "rust-injection"))]
    {
        // Silence unused-import warnings on the feature build.
        let _ = EngineEnvGuard::set;
    }
}

#[test]
fn supervisor_phase_b_falls_back_when_real_backend_unavailable() {
    // F2 (Codex P1 PR #519 in_process.rs:373): on a build that HAS
    // `rust-hotkeys` + `rust-injection` but lacks the whisper +
    // audio features, the in-process install must return
    // `MissingBackend` and the supervisor MUST fall back to the
    // Python worker path. Without this the older silent-stub
    // fallback in `rust_session_sink::build_production_sink` would
    // install a no-op sink that returns empty transcriptions on
    // every PTT press and the advertised auto-fallback would never
    // fire.
    //
    // Feature matrix this test covers:
    //   - rust-hotkeys + rust-injection but NOT whisper-rs-local +
    //     audio-in-rust: `try_build_production_sink` returns the
    //     "features required" error string; supervisor falls back.
    //
    // The stock-build case (no rust-hotkeys/rust-injection) is
    // covered by `supervisor_phase_b_stock_build_falls_back_to_python_on_engine_rust`
    // above. The all-features case is not exercised here because it
    // would actually install the in-process runtime, which needs a
    // display + audio + a live rdev listener the CI harness cannot
    // provide.
    #[cfg(all(
        feature = "rust-hotkeys",
        feature = "rust-injection",
        not(all(feature = "whisper-rs-local", feature = "audio-in-rust"))
    ))]
    {
        let Some(python) = test_python() else {
            return;
        };
        let _guard = EngineEnvGuard::set("rust");
        let mut supervisor = RuntimeSupervisor::new();
        supervisor
            .start(WorkerCommand {
                program: python,
                args: vec![
                    "-c".to_owned(),
                    "print('missing-backend-fallback-ran', flush=True)".to_owned(),
                ],
                working_dir: env::current_dir().unwrap(),
                env: Vec::new(),
            })
            .unwrap();
        let events = collect_until(&mut supervisor, |events| {
            has_stdout(events, "missing-backend-fallback-ran") && has_exit(events)
        });
        // Python fallback ran.
        assert!(
            has_stdout(&events, "missing-backend-fallback-ran"),
            "Python fallback must run when the real backend cannot init"
        );
        // AND supervisor emitted the F2 fallback stderr line naming
        // the reason. The exact wording lives in
        // `InProcessInstallError::MissingBackend`'s Display impl.
        let saw_fallback_stderr = events.iter().any(|e| {
            matches!(
                e,
                RuntimeEvent::Stderr(line)
                    if line.contains("Phase B in-process dispatch refused")
                        && line.contains("cannot serve PTT")
            )
        });
        assert!(
            saw_fallback_stderr,
            "supervisor must log the MissingBackend fallback line; events: {events:#?}",
        );
    }
    #[cfg(any(
        not(all(feature = "rust-hotkeys", feature = "rust-injection")),
        all(feature = "whisper-rs-local", feature = "audio-in-rust"),
    ))]
    {
        // Nothing to exercise on this feature configuration; the
        // stock case is covered by the sibling test above, and the
        // fully-featured case cannot be driven from the CI harness.
        let _ = EngineEnvGuard::set;
    }
}

#[test]
fn supervisor_phase_b_engine_python_leaves_default_path_intact() {
    // Regression: an explicit `VOICEPI_DICTATE_ENGINE=python` MUST run
    // the exact same Python-worker code path as the unset case. This
    // guarantees the Phase A fallback path stays a live escape hatch
    // for the whole Phase B rollout window (design doc's non-goal
    // "Deleting the Phase A subprocess path").
    let Some(python) = test_python() else {
        return;
    };
    let _guard = EngineEnvGuard::set("python");
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('explicit-python-ran', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "explicit-python-ran") && has_exit(events)
    });
    assert!(has_stdout(&events, "explicit-python-ran"));
    // No Phase B stderr line should appear when engine=python.
    assert!(
        !events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Stderr(line) if line.contains("Phase B in-process dispatch")
        )),
        "engine=python must not trigger the Phase B code path"
    );
}

#[test]
fn supervisor_phase_b_unknown_engine_warns_and_falls_back() {
    // Contract: an unknown engine value logs a stderr warning naming
    // the raw value AND falls back to the Python worker. Mirrors the
    // Python-side `_dispatch_engine`'s behaviour for unknown values.
    let Some(python) = test_python() else {
        return;
    };
    let _guard = EngineEnvGuard::set("mojo");
    let mut supervisor = RuntimeSupervisor::new();
    supervisor
        .start(WorkerCommand {
            program: python,
            args: vec![
                "-c".to_owned(),
                "print('unknown-fallback-ran', flush=True)".to_owned(),
            ],
            working_dir: env::current_dir().unwrap(),
            env: Vec::new(),
        })
        .unwrap();
    let events = collect_until(&mut supervisor, |events| {
        has_stdout(events, "unknown-fallback-ran") && has_exit(events)
    });
    assert!(has_stdout(&events, "unknown-fallback-ran"));
    assert!(
        events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Stderr(line) if line.contains("Unknown VOICEPI_DICTATE_ENGINE") && line.contains("mojo")
        )),
        "supervisor must log the unknown-engine warning naming the raw value"
    );
}

fn test_python() -> Option<PathBuf> {
    for candidate in python_candidates() {
        if Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

#[cfg(windows)]
fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    false
}

#[cfg(windows)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

#[cfg(windows)]
impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }
}

#[cfg(windows)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}

fn python_candidates() -> &'static [&'static str] {
    if cfg!(windows) {
        &["py.exe", "py", "python.exe", "python"]
    } else {
        &["python3", "python"]
    }
}

fn collect_until(
    supervisor: &mut RuntimeSupervisor,
    predicate: impl Fn(&[RuntimeEvent]) -> bool,
) -> Vec<RuntimeEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        events.extend(supervisor.poll());
        if predicate(&events) {
            return events;
        }
        thread::sleep(Duration::from_millis(25));
    }
    events
}

fn has_stdout(events: &[RuntimeEvent], expected: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Stdout(line) if line == expected))
}

fn has_exit(events: &[RuntimeEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RuntimeEvent::Exited { .. }))
}

fn has_worker_status(events: &[RuntimeEvent], expected: &str) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::Worker(worker)
                if worker.event == "status" && worker.state.as_deref() == Some(expected)
        )
    })
}
