//! Foreground-window probe: returns the active window's title, process, and id.
//!
//! Ported to Rust so the in-process dictation engine can drive per-app
//! target-profile matching (Python parity for
//! `vp_inject._capture_windows_target` on Windows and
//! `vp_inject._capture_target_window`'s `xdotool` path on Linux X11 —
//! `vp_events._apply_profile_settings` then swaps per-utterance settings
//! based on the returned title / process). Introduced by parity blocker #5
//! of the engine assessment (rust-target-profile-matching branch).
//!
//! # Semantics
//!
//! - Every backend is **best-effort** and **non-fatal**: any failure (no
//!   compositor answer, missing helper, denied FFI call, XKB session, …)
//!   is swallowed and surfaces as an empty [`WindowInfo`]. The dictation
//!   session then simply does not apply any profile for that utterance.
//! - The value is captured at the moment of the probe call; it is a
//!   **snapshot**, not a live stream. Callers re-probe per-utterance
//!   (matching Python's `_capture_target_window` at each `_start`).
//! - Neither title nor process is normalised beyond trimming trailing
//!   NULs / whitespace — the matcher does casefold + substring itself
//!   (see [`crate::profiles::match_profile`]).
//!
//! # Per-OS matrix
//!
//! | Target | Backend                                                         | Fallback |
//! |--------|-----------------------------------------------------------------|----------|
//! | Windows | `GetForegroundWindow` + `GetWindowTextW` + `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` via raw FFI (no crate deps) | empty `WindowInfo` on any FFI failure |
//! | Linux X11 | `xdotool getactivewindow` + `getwindowname` subprocess (mirrors `vp_inject._capture_target_window`) | empty `WindowInfo` when `xdotool` is missing or `DISPLAY` is unset |
//! | Linux Wayland | not implemented (compositors expose no portable "focused window" API today; matches Python, which also skips Wayland) | empty `WindowInfo` |
//! | macOS | not implemented (Python is a no-op on macOS too — no `_capture_target_window` branch) | empty `WindowInfo` |
//!
//! Tests exercise the platform-specific code paths behind `#[cfg]` guards
//! so the shared logic (trim, encoding, empty-normalisation) still runs on
//! every host.

use std::fmt;

/// Snapshot of the active foreground window's identifying strings. Both
/// fields are `None` when the platform probe could not resolve them (the
/// common case on Wayland and macOS; the default on any FFI failure).
///
/// Consumers pass the two `as_deref()` slices to
/// [`crate::profiles::match_profile`], which itself treats `None` and
/// empty exactly the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// Window title as reported by the compositor / desktop shell. Trailing
    /// NULs and outer whitespace are stripped; an empty result becomes `None`
    /// so a matcher rule keying on title behaves the same for "no title" and
    /// "unknown window".
    pub title: Option<String>,
    /// Executable basename or filename that owns the focused window on
    /// Windows (e.g. `WindowsTerminal.exe`). X11 does not expose the owning
    /// process reliably via `xdotool` so it stays `None` on that path. As
    /// with `title`, an empty result becomes `None`.
    pub process: Option<String>,
    /// Stable platform window identifier used when a later title change makes
    /// a title-only lookup unsafe (for example, an editor adding a dirty mark).
    pub target_id: Option<String>,
}

impl WindowInfo {
    /// True when the probe could not resolve a title, process, or platform id.
    /// The dictate session uses this to skip profile matching for the current
    /// utterance instead of asking the matcher with an empty snapshot.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.process.is_none() && self.target_id.is_none()
    }

    /// Convenience for tests + explicit construction. Empty / whitespace-only
    /// strings collapse to `None` so callers can pass raw FFI output without
    /// re-implementing the same trim rule.
    pub fn new(title: Option<String>, process: Option<String>) -> Self {
        Self {
            title: normalise(title),
            process: normalise(process),
            target_id: None,
        }
    }

    /// Attach a platform window identifier to an already-normalised snapshot.
    pub fn with_target_id(mut self, target_id: Option<String>) -> Self {
        self.target_id = normalise(target_id);
        self
    }
}

impl fmt::Display for WindowInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "title={:?} process={:?} target_id={:?}",
            self.title.as_deref().unwrap_or(""),
            self.process.as_deref().unwrap_or(""),
            self.target_id.as_deref().unwrap_or(""),
        )
    }
}

fn normalise(value: Option<String>) -> Option<String> {
    let raw = value?;
    let trimmed = raw.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Live foreground-window probe. Two-method trait so tests can plug a
/// deterministic implementation (see [`FixedForegroundWindow`]) without
/// touching the OS.
pub trait ForegroundWindowProbe: Send + Sync {
    /// Best-effort snapshot of the currently-focused window. Never panics,
    /// never returns an error — a failure surfaces as an empty
    /// [`WindowInfo`] so callers do not have to plumb a Result through
    /// per-utterance hot paths.
    fn probe(&self) -> WindowInfo;
}

/// Deterministic probe used by tests + downstream call sites that want to
/// stamp a fixed value (e.g. an OS-picker UI that already resolved the
/// window itself).
#[derive(Debug, Clone, Default)]
pub struct FixedForegroundWindow {
    info: WindowInfo,
}

impl FixedForegroundWindow {
    /// Build a probe that always returns `info`.
    pub fn new(info: WindowInfo) -> Self {
        Self { info }
    }

    /// Convenience for the common `(title, process)` shape.
    pub fn from_parts(title: Option<&str>, process: Option<&str>) -> Self {
        Self::new(WindowInfo::new(
            title.map(str::to_owned),
            process.map(str::to_owned),
        ))
    }
}

impl ForegroundWindowProbe for FixedForegroundWindow {
    fn probe(&self) -> WindowInfo {
        self.info.clone()
    }
}

/// Default probe: whichever backend this build's target OS supplies. Uses
/// the raw-FFI Windows path on Windows, the xdotool subprocess on Linux (X11
/// only — Wayland collapses to `WindowInfo::default()`), and returns an
/// empty snapshot on macOS + every other target.
///
/// The struct itself is a Zero-Sized Type (unit struct), so wrapping it in
/// `Box<dyn ForegroundWindowProbe>` costs nothing beyond the vtable pointer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemForegroundWindow;

impl ForegroundWindowProbe for SystemForegroundWindow {
    fn probe(&self) -> WindowInfo {
        imp::probe()
    }
}

// ── Windows ────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod imp {
    //! Direct FFI to `user32` + `kernel32`, mirroring the ctypes calls in
    //! `vp_inject._capture_windows_target`. We avoid taking a new dep on the
    //! `windows` crate (already gated behind `audio-capture`) so the probe is
    //! available in every build config.

    use std::ffi::c_void;
    use std::os::raw::{c_int, c_ulong};

    use super::{normalise, WindowInfo};

    const PROCESS_QUERY_LIMITED_INFORMATION: c_ulong = 0x0000_1000;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> *mut c_void;
        fn GetWindowTextLengthW(hwnd: *mut c_void) -> c_int;
        fn GetWindowTextW(hwnd: *mut c_void, out: *mut u16, cch: c_int) -> c_int;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, lpdw: *mut c_ulong) -> c_ulong;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: c_ulong, inherit: c_int, pid: c_ulong) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> c_int;
        fn QueryFullProcessImageNameW(
            handle: *mut c_void,
            flags: c_ulong,
            buf: *mut u16,
            size: *mut c_ulong,
        ) -> c_int;
    }

    pub(super) fn probe() -> WindowInfo {
        // SAFETY: All calls are direct Win32 FFI on documented signatures.
        // We check every returned pointer / handle / length before using it,
        // and we own the buffers we hand across the boundary. `CloseHandle`
        // is the counterpart to `OpenProcess`; unbalanced closes are guarded
        // by the RAII-style scope below (the two OS calls are next to each
        // other so no additional cleanup path is possible).
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return WindowInfo::default();
            }
            let title = read_window_title(hwnd);
            let mut pid: c_ulong = 0;
            GetWindowThreadProcessId(hwnd, &mut pid as *mut c_ulong);
            let process = read_window_process(hwnd);
            let target_id = if pid == 0 {
                (hwnd as usize).to_string()
            } else {
                format!("{}:{pid}", hwnd as usize)
            };
            WindowInfo {
                title: normalise(title),
                process: normalise(process),
                target_id: Some(target_id),
            }
        }
    }

    unsafe fn read_window_title(hwnd: *mut c_void) -> Option<String> {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        // +1 for the terminating NUL Windows writes into the buffer.
        let cap = (len as usize).saturating_add(1);
        let mut buf: Vec<u16> = vec![0; cap];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), cap as c_int);
        if copied <= 0 {
            return None;
        }
        let end = (copied as usize).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    unsafe fn read_window_process(hwnd: *mut c_void) -> Option<String> {
        let mut pid: c_ulong = 0;
        GetWindowThreadProcessId(hwnd, &mut pid as *mut c_ulong);
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        // MAX_PATH plus a bit of slack. Windows may return the long-form path
        // for symlinked processes; a healthy 1024 covers every case we care
        // about (basename extraction handles the rest).
        let mut buf: Vec<u16> = vec![0; 1024];
        let mut size: c_ulong = buf.len() as c_ulong;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }
        let end = (size as usize).min(buf.len());
        let full = String::from_utf16_lossy(&buf[..end]);
        Some(basename(&full))
    }

    /// Return the basename of a Windows path. Split out so the parity with
    /// Python's `_windows_process_name_shared` (which uses `os.path.basename`)
    /// is unit-testable without the OS involved.
    pub(super) fn basename(path: &str) -> String {
        path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn basename_extracts_the_last_windows_segment() {
            assert_eq!(
                basename("C:\\Program Files\\WindowsTerminal\\wt.exe"),
                "wt.exe"
            );
        }

        #[test]
        fn basename_handles_forward_slashes_and_bare_names() {
            assert_eq!(basename("/usr/bin/foo"), "foo");
            assert_eq!(basename("Explorer.exe"), "Explorer.exe");
            assert_eq!(basename(""), "");
        }

        #[test]
        fn probe_never_panics_and_returns_something_or_default() {
            // We cannot assert on the returned title/process in CI (there is
            // no active window under a headless runner). The contract we DO
            // pin: the call must not panic AND the returned WindowInfo must
            // either be empty or carry trimmed strings.
            let info = probe();
            if let Some(title) = info.title.as_deref() {
                assert_eq!(title.trim(), title);
                assert!(!title.is_empty());
            }
            if let Some(process) = info.process.as_deref() {
                assert_eq!(process.trim(), process);
                assert!(!process.is_empty());
            }
        }
    }
}

// ── Linux ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod imp {
    //! Linux backend: shells out to `xdotool` on X11 (matches the Python
    //! implementation in `vp_inject._capture_target_window`). Wayland has no
    //! portable "focused window" API today; the probe returns an empty
    //! [`WindowInfo`] there, and the dictate session falls back to the
    //! default profile — the same behaviour the Python worker exhibits on
    //! Wayland.

    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::{normalise, WindowInfo};

    pub(super) fn probe() -> WindowInfo {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none() {
            // Pure Wayland (no XWayland DISPLAY) — xdotool cannot help here
            // and every desktop-environment API is compositor-specific. Match
            // Python's behaviour on this path: skip.
            return WindowInfo::default();
        }
        let Some(xwin) = run_xdotool(&["getactivewindow"]) else {
            return WindowInfo::default();
        };
        let xwin = xwin.trim();
        let title = run_xdotool(&["getwindowname", xwin]);
        let pid = run_xdotool(&["getwindowpid", xwin]);
        x11_window_info(xwin, title, pid)
    }

    fn x11_window_info(xwin: &str, title: Option<String>, pid: Option<String>) -> WindowInfo {
        let Some(title) = normalise(title) else {
            return WindowInfo::default();
        };
        if xwin.trim().is_empty() {
            return WindowInfo::default();
        }
        let target_id = normalise(pid)
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|pid| *pid != 0)
            .map(|pid| format!("{}:{pid}", xwin.trim()));
        WindowInfo {
            title: Some(title),
            process: None,
            target_id,
        }
    }

    fn run_xdotool(args: &[&str]) -> Option<String> {
        // Guard on PATH lookup ourselves so a missing xdotool short-circuits
        // WITHOUT spawning a process (no stderr noise, no fork cost).
        which("xdotool")?;
        let mut cmd = Command::new("xdotool");
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let output = spawn_with_timeout(&mut cmd, Duration::from_secs(1))?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    /// Bare `which`: scan PATH for an executable file with the given basename.
    /// Split out so [`probe`] does not depend on a shell fork just to
    /// determine helper presence.
    fn which(name: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// Small `wait_timeout` shim: spawn the child, join it with an upper
    /// bound so a hung xdotool cannot wedge PTT press. On timeout the child
    /// is killed and `None` is returned.
    fn spawn_with_timeout(cmd: &mut Command, timeout: Duration) -> Option<std::process::Output> {
        let mut child = cmd.spawn().ok()?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    return child.wait_with_output().ok();
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn probe_never_panics_and_returns_normalised_strings() {
            // No live X11 in CI: assert only the contract (no panic, either
            // empty or trimmed non-empty strings).
            let info = probe();
            if let Some(title) = info.title.as_deref() {
                assert_eq!(title.trim(), title);
                assert!(!title.is_empty());
            }
            assert!(info.process.is_none(), "linux backend never sets process");
        }

        #[test]
        fn which_returns_some_for_a_shell_that_must_exist() {
            // POSIX guarantees `sh` on PATH. If this ever fails the runner
            // is not a Linux shell and this test file is misconfigured.
            assert!(which("sh").is_some());
        }

        #[test]
        fn which_returns_none_for_a_bogus_name() {
            assert!(which("this-binary-does-not-exist-xyz").is_none());
        }

        #[test]
        fn x11_target_requires_a_nonempty_title_and_id() {
            assert!(x11_window_info("123", None, Some("456".to_owned())).is_empty());
            assert!(
                x11_window_info("", Some("Editor".to_owned()), Some("456".to_owned())).is_empty()
            );
            let info = x11_window_info(
                " 123 ",
                Some(" Editor ".to_owned()),
                Some(" 456 ".to_owned()),
            );
            assert_eq!(info.title.as_deref(), Some("Editor"));
            assert_eq!(info.target_id.as_deref(), Some("123:456"));
            assert!(x11_window_info("123", Some("Editor".to_owned()), None)
                .target_id
                .is_none());
        }
    }
}

// ── macOS + fallback ──────────────────────────────────────────────────────

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod imp {
    //! macOS + any other target: no probe. Matches Python, which has no
    //! `_capture_target_window` branch on macOS. Callers see the default
    //! [`WindowInfo`] and simply skip profile matching for the utterance.

    use super::WindowInfo;

    pub(super) fn probe() -> WindowInfo {
        WindowInfo::default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn probe_is_a_no_op() {
            assert_eq!(probe(), WindowInfo::default());
        }
    }
}

// ── shared tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_info_new_trims_and_normalises_empty_to_none() {
        let info = WindowInfo::new(Some("  Editor \0".to_owned()), Some("   ".to_owned()));
        assert_eq!(info.title.as_deref(), Some("Editor"));
        assert_eq!(info.process, None);
    }

    #[test]
    fn is_empty_true_when_both_fields_none() {
        let info = WindowInfo::default();
        assert!(info.is_empty());
    }

    #[test]
    fn is_empty_false_when_any_field_set() {
        let info = WindowInfo::new(Some("Editor".to_owned()), None);
        assert!(!info.is_empty());
        let info = WindowInfo::new(None, Some("code".to_owned()));
        assert!(!info.is_empty());
        let info = WindowInfo::default().with_target_id(Some("123:456".to_owned()));
        assert!(!info.is_empty());
    }

    #[test]
    fn fixed_probe_returns_stamped_value_verbatim() {
        let probe = FixedForegroundWindow::from_parts(Some("Editor"), Some("code"));
        assert_eq!(probe.probe().title.as_deref(), Some("Editor"));
        assert_eq!(probe.probe().process.as_deref(), Some("code"));
    }

    #[test]
    fn fixed_probe_default_is_empty() {
        let probe = FixedForegroundWindow::default();
        assert!(probe.probe().is_empty());
    }

    #[test]
    fn system_probe_never_panics() {
        // Contract test: on every platform, the default probe must return
        // *something* (empty or populated) without panicking.
        let _ = SystemForegroundWindow.probe();
    }
}
