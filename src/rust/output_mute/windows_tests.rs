//! Mock PowerShell runner + WindowsBackend unit tests.
//!
//! Codex P2 (windows.rs:96, PR #440) — pulled out of `windows.rs` (which
//! was already at 493 lines and would exceed AGENTS.md's ~500-LOC gate
//! after the multi-role rework) so the impl file stays under the cap.
//! Wired in via `#[path = "windows_tests.rs"] mod tests;` from
//! `windows.rs`.

use super::*;
use std::sync::{Arc, Mutex as StdMutex};

use crate::output_mute::MuteController;

#[derive(Default)]
struct MockShell {
    inner: StdMutex<MockState>,
}

#[derive(Default)]
struct MockState {
    /// Prior mute state per role (0=eConsole, 1=eMultimedia,
    /// 2=eCommunications). The script's `set_all` action writes into
    /// these; `get_all` reads from them.
    per_role: [bool; 3],
    /// One entry per `run()` call, in order. Each entry captures the
    /// action string and the `VOICEPI_MUTE_TARGETS` value that was
    /// passed (`None` when the action didn't need one).
    calls: Vec<(String, Option<String>)>,
    spawn_failure: Option<MuteError>,
    force_failure_exit: bool,
}

impl PowerShellRunner for Arc<MockShell> {
    fn run(
        &self,
        _script: &str,
        action: &str,
        targets: Option<&str>,
    ) -> Result<PowerShellResult, MuteError> {
        let mut state = self.inner.lock().unwrap();
        state
            .calls
            .push((action.to_owned(), targets.map(str::to_owned)));
        if let Some(err) = state.spawn_failure.take() {
            return Err(err);
        }
        if state.force_failure_exit {
            return Ok(PowerShellResult {
                status_ok: false,
                stdout: String::new(),
                stderr: "forced mock ps failure".to_owned(),
            });
        }
        match action {
            "get_all" => {
                let mut stdout = String::new();
                for (r, muted) in state.per_role.iter().enumerate() {
                    stdout.push_str(&format!("ROLE:{r}:{}\n", if *muted { 1 } else { 0 }));
                }
                stdout.push_str("OK\n");
                Ok(PowerShellResult {
                    status_ok: true,
                    stdout,
                    stderr: String::new(),
                })
            }
            "set_all" => {
                if let Some(targets) = targets {
                    let items: Vec<&str> = targets.split(',').collect();
                    for (r, item) in items.iter().enumerate() {
                        if r >= 3 {
                            break;
                        }
                        match item.trim() {
                            "1" => state.per_role[r] = true,
                            "0" => state.per_role[r] = false,
                            _ => {}
                        }
                    }
                }
                Ok(PowerShellResult {
                    status_ok: true,
                    stdout: "OK\n".to_owned(),
                    stderr: String::new(),
                })
            }
            other => Ok(PowerShellResult {
                status_ok: false,
                stdout: String::new(),
                stderr: format!("mock ps: unexpected action {other}"),
            }),
        }
    }
}

impl MockShell {
    fn set_per_role(&self, per_role: [bool; 3]) {
        self.inner.lock().unwrap().per_role = per_role;
    }
    fn per_role(&self) -> [bool; 3] {
        self.inner.lock().unwrap().per_role
    }
    fn actions(&self) -> Vec<(String, Option<String>)> {
        self.inner.lock().unwrap().calls.clone()
    }
    fn set_spawn_failure(&self, err: MuteError) {
        self.inner.lock().unwrap().spawn_failure = Some(err);
    }
    fn set_force_failure_exit(&self, on: bool) {
        self.inner.lock().unwrap().force_failure_exit = on;
    }
}

fn backend(mock: Arc<MockShell>) -> WindowsBackend<Arc<MockShell>> {
    WindowsBackend::with_runner(mock)
}

#[test]
fn parse_get_all_reads_three_role_lines() {
    let text = "ROLE:0:0\nROLE:1:1\nROLE:2:0\nOK\n";
    let snap = parse_get_all(text).unwrap();
    assert_eq!(snap.per_role, [false, true, false]);
}

#[test]
fn parse_get_all_reports_missing_role() {
    // Only two roles present -> UnexpectedOutput.
    let text = "ROLE:0:0\nROLE:1:1\nOK\n";
    let err = parse_get_all(text).unwrap_err();
    assert!(matches!(err, MuteError::UnexpectedOutput(_)));
}

#[test]
fn parse_get_all_reports_bad_role_value() {
    let text = "ROLE:0:maybe\nROLE:1:0\nROLE:2:0\n";
    let err = parse_get_all(text).unwrap_err();
    assert!(matches!(err, MuteError::UnexpectedOutput(_)));
}

#[test]
fn parse_get_all_reports_out_of_range_role() {
    let text = "ROLE:3:0\nROLE:1:0\nROLE:2:0\n";
    let err = parse_get_all(text).unwrap_err();
    assert!(matches!(err, MuteError::UnexpectedOutput(_)));
}

#[test]
fn pin_current_endpoint_captures_all_three_role_states() {
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([true, false, true]);
    let backend = backend(mock.clone());

    backend.pin_current_endpoint().unwrap();
    // A get_mute after pin returns "all muted?" from the snapshot.
    assert!(!backend.get_mute().unwrap(), "one role unmuted at pin time");
    // Only one PowerShell round-trip (the get_all done by pin).
    let calls = mock.actions();
    assert_eq!(
        calls,
        vec![("get_all".to_owned(), None),],
        "get_mute must reuse the snapshot rather than re-query"
    );
}

#[test]
fn get_mute_returns_true_only_when_all_roles_muted() {
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([true, true, true]);
    let backend = backend(mock.clone());
    backend.pin_current_endpoint().unwrap();
    assert!(backend.get_mute().unwrap(), "all three roles muted -> true");
}

#[test]
fn set_mute_true_mutes_all_three_roles() {
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([false, false, false]);
    let backend = backend(mock.clone());
    backend.pin_current_endpoint().unwrap();
    backend.set_mute(true).unwrap();
    assert_eq!(mock.per_role(), [true, true, true]);
    // set_all must carry "1,1,1" targets.
    let last = mock.actions().last().cloned().unwrap();
    assert_eq!(last.0, "set_all");
    assert_eq!(last.1.as_deref(), Some("1,1,1"));
}

#[test]
fn set_mute_false_restores_each_role_to_its_prior_state() {
    // Prior: eConsole muted, eMultimedia unmuted, eCommunications muted.
    // We call set_mute(true) (mutes all), then set_mute(false) must
    // restore each role to its ORIGINAL state — NOT blanket-unmute.
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([true, false, true]);
    let backend = backend(mock.clone());
    backend.pin_current_endpoint().unwrap();
    backend.set_mute(true).unwrap();
    assert_eq!(mock.per_role(), [true, true, true]);
    backend.set_mute(false).unwrap();
    // The restore must reproduce [true, false, true].
    assert_eq!(
        mock.per_role(),
        [true, false, true],
        "restore must return each endpoint to its ORIGINAL mute state, \
         not blanket-unmute (Codex P2 windows.rs:96)"
    );
    let calls = mock.actions();
    let restore = calls.last().unwrap();
    assert_eq!(restore.0, "set_all");
    assert_eq!(
        restore.1.as_deref(),
        Some("1,0,1"),
        "restore must carry the original per-role targets"
    );
}

#[test]
fn clear_endpoint_pin_drops_the_snapshot() {
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([true, false, true]);
    let backend = backend(mock.clone());
    backend.pin_current_endpoint().unwrap();
    backend.clear_endpoint_pin();
    // A follow-up get_mute must re-query (no snapshot).
    mock.set_per_role([false, false, false]);
    assert!(!backend.get_mute().unwrap(), "no snapshot -> live query");
    let calls = mock.actions();
    assert_eq!(calls.iter().filter(|(a, _)| a == "get_all").count(), 2);
}

#[test]
fn spawn_failure_becomes_unavailable() {
    let mock = Arc::new(MockShell::default());
    mock.set_spawn_failure(MuteError::Unavailable("no powershell".to_owned()));
    let backend = backend(mock.clone());
    assert!(matches!(
        backend.pin_current_endpoint().unwrap_err(),
        MuteError::Unavailable(_)
    ));
}

#[test]
fn nonzero_exit_becomes_os_failure() {
    let mock = Arc::new(MockShell::default());
    mock.set_force_failure_exit(true);
    let backend = backend(mock.clone());
    assert!(matches!(
        backend.pin_current_endpoint().unwrap_err(),
        MuteError::OsFailure(_)
    ));
    // Reset failure so we can pre-populate a snapshot for set_mute.
    mock.set_force_failure_exit(false);
    backend.pin_current_endpoint().unwrap();
    mock.set_force_failure_exit(true);
    assert!(matches!(
        backend.set_mute(true).unwrap_err(),
        MuteError::OsFailure(_)
    ));
}

#[test]
fn set_all_without_ok_becomes_unexpected_output() {
    // A run that succeeded but printed nothing useful — treat as
    // malformed so we do not silently claim to have muted.
    struct SilentRunner;
    impl PowerShellRunner for SilentRunner {
        fn run(
            &self,
            _script: &str,
            _action: &str,
            _targets: Option<&str>,
        ) -> Result<PowerShellResult, MuteError> {
            Ok(PowerShellResult {
                status_ok: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }
    let backend = WindowsBackend::with_runner(SilentRunner);
    assert!(matches!(
        backend.set_mute(true).unwrap_err(),
        MuteError::UnexpectedOutput(_)
    ));
}

#[test]
fn controller_full_recording_cycle_uses_get_all_then_set_all_pair() {
    // End-to-end at the controller layer: pin captures the initial
    // per-role snapshot, set_mute(true) mutes all three, set_mute(false)
    // restores each role to its original state.
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([false, true, false]);
    let backend = Arc::new(WindowsBackend::with_runner(mock.clone()));
    let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

    controller.on_recording_start();
    assert_eq!(
        mock.per_role(),
        [true, true, true],
        "start must mute every role default"
    );
    controller.on_recording_stop();
    assert_eq!(
        mock.per_role(),
        [false, true, false],
        "stop must restore each role's ORIGINAL mute state"
    );

    let actions: Vec<String> = mock.actions().into_iter().map(|(a, _)| a).collect();
    assert_eq!(
        actions,
        vec![
            "get_all".to_owned(), // pin_current_endpoint
            "set_all".to_owned(), // set_mute(true) -> 1,1,1
            "set_all".to_owned(), // set_mute(false) -> restore
        ],
        "controller drives one get_all + two set_all across a cycle"
    );
}

#[test]
fn controller_skips_restore_when_all_roles_were_already_muted() {
    // If the user had every role default muted before recording, the
    // controller must not touch anything: no set_all at all.
    let mock = Arc::new(MockShell::default());
    mock.set_per_role([true, true, true]);
    let backend = Arc::new(WindowsBackend::with_runner(mock.clone()));
    let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

    controller.on_recording_start();
    controller.on_recording_stop();

    // Only the pin's get_all round-trip — no mute/unmute.
    let actions: Vec<String> = mock.actions().into_iter().map(|(a, _)| a).collect();
    assert_eq!(actions, vec!["get_all".to_owned()]);
    assert_eq!(mock.per_role(), [true, true, true]);
}

#[test]
fn powershell_script_contains_the_coreaudio_interface_signatures() {
    // Sanity-check the embedded script so a careless edit that
    // removes the SetMute/GetMute lines fails the build rather
    // than showing up as a runtime error on Windows only.
    assert!(POWERSHELL_SCRIPT.contains("SetMute"));
    assert!(POWERSHELL_SCRIPT.contains("GetMute"));
    assert!(POWERSHELL_SCRIPT.contains("IAudioEndpointVolume"));
    assert!(POWERSHELL_SCRIPT.contains("IMMDeviceEnumerator"));
}

#[test]
fn iaudio_vtable_places_setmute_and_getmute_in_the_correct_slots() {
    // Codex P1 (windows.rs:51, PR #440) — SetMute must sit
    // immediately after the last NotUsed11 slot (which stands in
    // for GetChannelVolumeLevelScalar in the real vtable) and
    // GetMute must sit immediately after SetMute. A stray
    // NotUsed12 shim used to shift both methods into the wrong
    // vtable slots. Pin the interface signature so a similar
    // future edit fails CI instead of showing up as a silent
    // no-op on Windows only.
    let script = POWERSHELL_SCRIPT;
    let not_used11 = script.find("NotUsed11").expect("NotUsed11 slot present");
    let set_mute = script
        .find("int SetMute([MarshalAs")
        .expect("SetMute signature present");
    let get_mute = script
        .find("int GetMute([MarshalAs")
        .expect("GetMute signature present");
    assert!(
        not_used11 < set_mute && set_mute < get_mute,
        "SetMute must follow the last NotUsed11 slot, GetMute must follow SetMute",
    );

    // The stray NotUsed12 placeholder must be gone.
    let between = &script[not_used11..set_mute];
    assert!(
        !between.contains("NotUsed12"),
        "no NotUsed12 stub is allowed between the volume methods and SetMute",
    );
}

#[test]
fn getmute_out_parameter_is_marshalled_as_win32_bool() {
    // Codex P2 (windows.rs:53, PR #440) — see the tests-file-level
    // comment on prior behaviour. Guard the marshalling attribute.
    assert!(
        POWERSHELL_SCRIPT.contains("int GetMute([MarshalAs(UnmanagedType.Bool)] out bool"),
        "GetMute's out parameter must be explicitly marshalled as Win32 BOOL",
    );
}

#[test]
fn powershell_actions_check_hresults_before_reporting_success() {
    // Codex P2 (windows.rs:87, PR #440) — every SetMute/GetMute must
    // have its HRESULT checked. The multi-role script wraps the calls
    // slightly differently (via Marshal.ThrowExceptionForHR($vol.SetMute(...))),
    // so pin the wrappers rather than the exact argument list.
    let script = POWERSHELL_SCRIPT;
    for needle in [
        "ThrowExceptionForHR($vol.GetMute",
        "ThrowExceptionForHR($vol.SetMute",
    ] {
        assert!(
            script.contains(needle),
            "powershell script must wrap `{needle}...` in Marshal.ThrowExceptionForHR",
        );
    }
    // Belt-and-braces: the previous [void]$vol.Set/GetMute pattern
    // (which discarded the HRESULT) must not creep back in.
    assert!(!script.contains("[void]$vol.SetMute"));
    assert!(!script.contains("[void]$vol.GetMute"));
}

#[test]
fn powershell_script_iterates_all_three_roles() {
    // Codex P2 (windows.rs:96, PR #440): every render role default
    // endpoint must be touched. Pin the loop structure so a future
    // refactor cannot silently drop one.
    let script = POWERSHELL_SCRIPT;
    assert!(
        script.contains("ActivateForRole($r)"),
        "script must activate per-role default endpoints"
    );
    assert!(
        script.contains("$r -lt 3"),
        "script must iterate roles 0, 1, 2"
    );
}
