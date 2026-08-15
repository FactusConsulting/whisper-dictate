//! Process-isolated hotkey probe used by the desktop guided verifier.
//!
//! `rdev::listen` and the evdev reader cannot always terminate their OS
//! listener thread in-process. Running the bounded chord-only capture in the
//! sibling `wd` process makes process exit the teardown boundary, so repeated
//! diagnostics never accumulate global hooks in the GUI process.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

const PROBE_DURATION_SECS: &str = "86400";
const MAX_DIAGNOSTIC_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyProbeSignal {
    Press,
    Release,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HotkeyProbeEvent {
    Installed {
        driver: String,
        chord: String,
    },
    Signal {
        signal: HotkeyProbeSignal,
        focused: Option<bool>,
    },
    Diagnostic(String),
    Failed(String),
    Exited(Option<i32>),
}

pub(crate) struct HotkeyProbe {
    child: Option<Child>,
    rx: Receiver<HotkeyProbeEvent>,
    readers: Vec<JoinHandle<()>>,
    exit_reported: bool,
}

impl HotkeyProbe {
    pub(crate) fn spawn(
        chord: &str,
        driver: &str,
        repaint: super::RepaintNotifier,
    ) -> Result<Self, String> {
        let mut command = probe_command(chord, driver);
        let mut child = command
            .spawn()
            .map_err(|err| format!("could not start the hotkey diagnostic process: {err}"))?;
        let Some(stdout) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err("hotkey diagnostic stdout pipe was unavailable".to_owned());
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_child(&mut child);
            return Err("hotkey diagnostic stderr pipe was unavailable".to_owned());
        };
        let (tx, rx) = mpsc::channel();
        let stdout_reader = match spawn_stdout_reader(stdout, tx.clone(), Arc::clone(&repaint)) {
            Ok(reader) => reader,
            Err(reason) => {
                terminate_child(&mut child);
                return Err(reason);
            }
        };
        let stderr_reader = match spawn_stderr_reader(stderr, tx, repaint) {
            Ok(reader) => reader,
            Err(reason) => {
                terminate_child(&mut child);
                let _ = stdout_reader.join();
                return Err(reason);
            }
        };
        Ok(Self {
            child: Some(child),
            rx,
            readers: vec![stdout_reader, stderr_reader],
            exit_reported: false,
        })
    }

    pub(crate) fn poll(&mut self) -> Vec<HotkeyProbeEvent> {
        let mut events = self.rx.try_iter().collect::<Vec<_>>();
        if !self.exit_reported {
            let status = self
                .child
                .as_mut()
                .and_then(|child| match child.try_wait() {
                    Ok(Some(status)) => Some(Ok(status.code())),
                    Ok(None) => None,
                    Err(err) => Some(Err(err)),
                });
            match status {
                Some(Ok(code)) => {
                    self.exit_reported = true;
                    self.child.take();
                    self.join_readers();
                    events.extend(self.rx.try_iter());
                    events.push(HotkeyProbeEvent::Exited(code));
                }
                Some(Err(err)) => {
                    self.exit_reported = true;
                    self.terminate();
                    events.extend(self.rx.try_iter());
                    events.push(HotkeyProbeEvent::Failed(format!(
                        "could not inspect hotkey diagnostic process: {err}"
                    )));
                }
                None => {}
            }
        }
        events
    }

    pub(crate) fn shutdown(mut self) {
        self.terminate();
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            terminate_child(&mut child);
        }
        self.join_readers();
    }

    fn join_readers(&mut self) {
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn terminate_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

impl Drop for HotkeyProbe {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn probe_args(chord: &str, driver: &str) -> Vec<OsString> {
    vec![
        OsString::from("hotkey"),
        OsString::from("capture"),
        OsString::from("--for"),
        OsString::from(PROBE_DURATION_SECS),
        OsString::from("--json"),
        OsString::from("--chord-events-only"),
        OsString::from("--focus-process"),
        OsString::from(std::process::id().to_string()),
        OsString::from("--driver"),
        OsString::from(driver),
        OsString::from("--chord"),
        OsString::from(chord),
    ]
}

fn probe_command(chord: &str, driver: &str) -> Command {
    let mut command = Command::new(super::cli_exe_path());
    command
        .args(probe_args(chord, driver))
        .env_remove("VOICEPI_HOTKEY_DEBUG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
}

fn spawn_stdout_reader(
    stdout: impl std::io::Read + Send + 'static,
    tx: Sender<HotkeyProbeEvent>,
    repaint: super::RepaintNotifier,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("hotkey-probe-stdout".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let event = match line {
                    Ok(line) => parse_stdout_line(&line),
                    Err(err) => Some(HotkeyProbeEvent::Failed(format!(
                        "could not read hotkey diagnostic output: {err}"
                    ))),
                };
                if let Some(event) = event {
                    let _ = tx.send(event);
                    repaint();
                }
            }
        })
        .map_err(|err| format!("could not start hotkey diagnostic output reader: {err}"))
}

fn spawn_stderr_reader(
    stderr: impl std::io::Read + Send + 'static,
    tx: Sender<HotkeyProbeEvent>,
    repaint: super::RepaintNotifier,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("hotkey-probe-stderr".to_owned())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    let _ = tx.send(HotkeyProbeEvent::Diagnostic(bounded_line(&line)));
                    repaint();
                }
            }
        })
        .map_err(|err| format!("could not start hotkey diagnostic error reader: {err}"))
}

fn parse_stdout_line(line: &str) -> Option<HotkeyProbeEvent> {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            return Some(HotkeyProbeEvent::Failed(format!(
                "hotkey diagnostic returned invalid JSON: {err}"
            )))
        }
    };
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("listener_installed") => match (
            value.get("driver").and_then(serde_json::Value::as_str),
            value.get("chord").and_then(serde_json::Value::as_str),
        ) {
            (Some(driver), Some(chord)) => Some(HotkeyProbeEvent::Installed {
                driver: driver.to_owned(),
                chord: chord.to_owned(),
            }),
            _ => Some(HotkeyProbeEvent::Failed(
                "hotkey diagnostic install event omitted driver or chord".to_owned(),
            )),
        },
        Some("chord_matched") => Some(HotkeyProbeEvent::Signal {
            signal: HotkeyProbeSignal::Press,
            focused: value.get("focused").and_then(serde_json::Value::as_bool),
        }),
        Some("chord_released") => Some(HotkeyProbeEvent::Signal {
            signal: HotkeyProbeSignal::Release,
            focused: value.get("focused").and_then(serde_json::Value::as_bool),
        }),
        Some("chord_canceled") => Some(HotkeyProbeEvent::Signal {
            signal: HotkeyProbeSignal::Cancel,
            focused: value.get("focused").and_then(serde_json::Value::as_bool),
        }),
        Some("duration_reached" | "exit_on_chord") => Some(HotkeyProbeEvent::Failed(
            "hotkey diagnostic ended before both focus checks completed".to_owned(),
        )),
        Some("key_down" | "key_up") | None => None,
        Some(_) => None,
    }
}

fn bounded_line(line: &str) -> String {
    let mut chars = line.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_request_chord_only_json_without_config_mutation() {
        let args = probe_args("ctrl+f9", "win_registerhotkey");
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["--chord", "ctrl+f9"]));
        assert!(args.contains(&std::borrow::Cow::Borrowed("--chord-events-only")));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "--focus-process" && pair[1] == std::process::id().to_string()
        }));
        assert!(!args.contains(&std::borrow::Cow::Borrowed("--config")));
    }

    #[test]
    fn command_suppresses_inherited_raw_key_debugging() {
        let command = probe_command("pause", "win_registerhotkey");
        let debug = command
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("VOICEPI_HOTKEY_DEBUG"));
        assert!(matches!(debug, Some((_, None))));
    }

    #[test]
    fn parser_preserves_each_event_source_focus_snapshot() {
        assert_eq!(
            parse_stdout_line(r#"{"kind":"chord_matched","id":1,"focused":false}"#),
            Some(HotkeyProbeEvent::Signal {
                signal: HotkeyProbeSignal::Press,
                focused: Some(false),
            })
        );
        assert_eq!(
            parse_stdout_line(r#"{"kind":"chord_released","id":1,"focused":true}"#),
            Some(HotkeyProbeEvent::Signal {
                signal: HotkeyProbeSignal::Release,
                focused: Some(true),
            })
        );
    }

    #[test]
    fn parser_reports_actual_installed_driver_and_ignores_raw_keys() {
        assert_eq!(
            parse_stdout_line(
                r#"{"kind":"listener_installed","driver":"rdev","chord":"ctrl_l+f9"}"#,
            ),
            Some(HotkeyProbeEvent::Installed {
                driver: "rdev".to_owned(),
                chord: "ctrl_l+f9".to_owned(),
            })
        );
        assert_eq!(
            parse_stdout_line(r#"{"kind":"key_down","name":"secret"}"#),
            None
        );
    }

    #[test]
    fn malformed_install_event_fails_instead_of_staying_on_starting_forever() {
        assert_eq!(
            parse_stdout_line(r#"{"kind":"listener_installed","driver":"rdev"}"#),
            Some(HotkeyProbeEvent::Failed(
                "hotkey diagnostic install event omitted driver or chord".to_owned()
            ))
        );
    }

    #[test]
    fn stdout_reader_keeps_distinct_producer_focus_values() {
        let input = concat!(
            "{\"kind\":\"chord_matched\",\"id\":1,\"focused\":false}\n",
            "{\"kind\":\"chord_released\",\"id\":1,\"focused\":true}\n"
        );
        let (tx, rx) = mpsc::channel();
        let repaint: super::super::RepaintNotifier = Arc::new(|| {});
        let reader = spawn_stdout_reader(input.as_bytes(), tx, repaint).unwrap();
        reader.join().unwrap();

        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                HotkeyProbeEvent::Signal {
                    signal: HotkeyProbeSignal::Press,
                    focused: Some(false),
                },
                HotkeyProbeEvent::Signal {
                    signal: HotkeyProbeSignal::Release,
                    focused: Some(true),
                },
            ]
        );
    }

    #[test]
    fn process_exit_drains_diagnostics_before_reporting_exited() {
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "echo actionable diagnostic 1>&2 & exit /b 7"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let mut child = Command::new("sh")
            .args(["-c", "printf 'actionable diagnostic\\n' >&2; exit 7"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = spawn_stderr_reader(stderr, tx, Arc::new(|| {})).unwrap();
        let expected_code = child.wait().unwrap().code();
        let mut probe = HotkeyProbe {
            child: Some(child),
            rx,
            readers: vec![reader],
            exit_reported: false,
        };

        let events = probe.poll();

        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            HotkeyProbeEvent::Diagnostic(line) if line.trim() == "actionable diagnostic"
        ));
        assert_eq!(events[1], HotkeyProbeEvent::Exited(expected_code));
    }

    #[test]
    fn diagnostics_are_bounded_on_character_boundaries() {
        let long = "ø".repeat(MAX_DIAGNOSTIC_CHARS + 1);
        let bounded = bounded_line(&long);
        assert!(bounded.ends_with("..."));
        assert_eq!(bounded.chars().count(), MAX_DIAGNOSTIC_CHARS + 3);
    }

    #[test]
    fn terminate_child_reaps_the_owned_process() {
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "ping", "-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        #[cfg(not(windows))]
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        terminate_child(&mut child);

        assert!(child.try_wait().unwrap().is_some());
    }
}
