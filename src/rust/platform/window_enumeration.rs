//! Visible top-level window enumeration for the Profiles picker.
//!
//! The Settings UI previously launched the Python worker with
//! `--list-windows`. This module owns the same Windows contract in-process:
//! visible titled windows only, executable basename when available, PID text
//! when the process image query fails, and an empty process when access to the
//! owning process is denied.

use serde::Serialize;
#[cfg(any(target_os = "windows", test))]
use std::sync::OnceLock;

/// One visible, titled top-level window shown by the Profiles picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibleWindow {
    pub title: String,
    pub process: String,
}

/// Enumerate visible top-level windows.
///
/// Windows is the only supported platform. Failures for individual windows
/// are non-fatal; only failure of the top-level enumeration is returned.
pub fn list_visible_windows() -> Result<Vec<VisibleWindow>, String> {
    imp::list_visible_windows()
}

/// Restore and activate the captured target window before a UI-triggered
/// reinjection. A stable platform id is preferred; title/process matching is
/// the fallback for older event payloads.
pub fn activate_window(title: &str, process: &str) -> Result<(), String> {
    imp::activate_window(None, title, process)
}

/// Activate a previously captured target, preferring its stable platform id.
pub fn activate_window_with_id(target_id: &str, title: &str, process: &str) -> Result<(), String> {
    imp::activate_window(
        (!target_id.trim().is_empty()).then_some(target_id),
        title,
        process,
    )
}

/// CLI adapter preserving the retired Python window-list JSON contract.
pub fn handle_list_windows() -> anyhow::Result<()> {
    crate::diag::log!("[windows] debug: native visible-window enumeration requested");
    match list_visible_windows() {
        Ok(windows) => {
            crate::diag::log!(
                "[windows] trace: native enumeration returned {} visible windows",
                windows.len()
            );
            println!("{}", serde_json::to_string(&windows)?);
            Ok(())
        }
        Err(error) => {
            println!("{}", serde_json::json!({ "error": error }));
            Err(anyhow::anyhow!(error))
        }
    }
}

#[cfg(any(target_os = "windows", test))]
fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

#[cfg(any(target_os = "windows", test))]
fn process_name_or_pid(pid: u32, image_path: Option<&str>) -> String {
    image_path
        .map(basename)
        .map(str::to_owned)
        .unwrap_or_else(|| pid.to_string())
}

fn normalized_window_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(any(target_os = "windows", test))]
fn window_matches(title: &str, process: &str, wanted_title: &str, wanted_process: &str) -> bool {
    if normalized_window_text(title).to_lowercase()
        != normalized_window_text(wanted_title).to_lowercase()
    {
        return false;
    }
    let wanted_process = wanted_process.trim();
    wanted_process.is_empty()
        || basename(process.trim()).eq_ignore_ascii_case(basename(wanted_process))
}

#[cfg(any(target_os = "windows", test))]
fn is_self_window(title: &str, process: Option<&str>) -> bool {
    let normalised_title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    static SELF_TITLE: OnceLock<regex::Regex> = OnceLock::new();
    let self_title = SELF_TITLE.get_or_init(|| {
        regex::Regex::new(r"^whisper-dictate(?:\s+\d.*)?$")
            .expect("self-window title regex must compile")
    });
    if self_title.is_match(&normalised_title) {
        return true;
    }

    let Some(process) = process else {
        return false;
    };
    matches!(
        basename(process.trim()).to_lowercase().as_str(),
        "whisper-dictate"
            | "whisper-dictate.exe"
            | "whisper-dictate-gui"
            | "whisper-dictate-gui.exe"
            | "wd"
            | "wd.exe"
            | "wd-gui"
            | "wd-gui.exe"
            | "whisper_dictate"
            | "whisper_dictate.exe"
    )
}

#[cfg(target_os = "windows")]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow,
        ShowWindow, SW_RESTORE,
    };

    use super::{basename, is_self_window, process_name_or_pid, window_matches, VisibleWindow};

    pub(super) fn list_visible_windows() -> Result<Vec<VisibleWindow>, String> {
        let mut windows = Vec::new();
        // SAFETY: `windows` lives for the entire synchronous EnumWindows call.
        // The callback casts LPARAM back to the same Vec type and always
        // returns TRUE, so one inaccessible window cannot stop enumeration.
        let ok = unsafe {
            EnumWindows(
                Some(enum_window),
                (&mut windows as *mut Vec<VisibleWindow>) as LPARAM,
            )
        };
        if ok == 0 {
            return Err(format!(
                "Windows window enumeration failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(windows)
    }

    pub(super) fn activate_window(
        target_id: Option<&str>,
        title: &str,
        process: &str,
    ) -> Result<(), String> {
        if title.trim().is_empty() {
            return Err("the previous target window has no title".to_owned());
        }
        if let Some(raw_id) = target_id {
            if let Some((value, captured_pid)) = parse_target_id(raw_id) {
                let hwnd = value as HWND;
                // HWND values can be reused after a window closes. Verify
                // the current owner before trusting the captured handle.
                if unsafe {
                    IsWindow(hwnd) != 0 && window_belongs_to_target(hwnd, captured_pid, process)
                } {
                    return activate_hwnd(hwnd, title);
                }
            }
        }
        let mut context = ActivationContext {
            title: title.to_owned(),
            process: process.to_owned(),
            hwnd: None,
        };
        // SAFETY: the callback receives a pointer to the live context for the
        // duration of the synchronous EnumWindows call.
        let enum_ok = unsafe {
            EnumWindows(
                Some(activate_window_callback),
                (&mut context as *mut ActivationContext) as LPARAM,
            )
        };
        if enum_ok == 0 && context.hwnd.is_none() {
            return Err(format!(
                "Windows window enumeration failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let Some(hwnd) = context.hwnd else {
            return Err(format!(
                "could not find the previous target window {:?}",
                title.trim()
            ));
        };
        // SAFETY: hwnd came from EnumWindows and remains valid for this
        // synchronous activation attempt.
        activate_hwnd(hwnd, title)
    }

    fn activate_hwnd(hwnd: HWND, title: &str) -> Result<(), String> {
        // SAFETY: hwnd came from a validated handle or EnumWindows and is
        // used only for this synchronous activation attempt.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            BringWindowToTop(hwnd);
            if SetForegroundWindow(hwnd) == 0 {
                return Err(format!(
                    "Windows refused to activate target window {:?}",
                    title.trim()
                ));
            }
        }
        Ok(())
    }

    fn parse_target_id(raw: &str) -> Option<(usize, Option<u32>)> {
        let mut parts = raw.trim().split(':');
        let hwnd = parts.next()?.parse::<usize>().ok()?;
        let pid = parts.next().map(str::parse).transpose().ok().flatten();
        if parts.next().is_some() {
            return None;
        }
        Some((hwnd, pid))
    }

    unsafe fn window_belongs_to_target(
        hwnd: HWND,
        captured_pid: Option<u32>,
        expected_process: &str,
    ) -> bool {
        let mut current_pid = 0;
        GetWindowThreadProcessId(hwnd, &mut current_pid);
        if current_pid == 0 {
            return false;
        }
        if let Some(captured_pid) = captured_pid {
            if current_pid != captured_pid {
                return false;
            }
        }
        let expected_process = expected_process.trim();
        if expected_process.is_empty() {
            return captured_pid.is_some();
        }
        if expected_process.parse::<u32>().ok() == Some(current_pid) {
            return true;
        }
        read_process_name(current_pid)
            .map(|actual| basename(actual.trim()).eq_ignore_ascii_case(basename(expected_process)))
            .unwrap_or(false)
    }

    struct ActivationContext {
        title: String,
        process: String,
        hwnd: Option<HWND>,
    }

    unsafe extern "system" fn activate_window_callback(hwnd: HWND, lparam: LPARAM) -> i32 {
        std::panic::catch_unwind(|| {
            // SAFETY: lparam points to the live ActivationContext owned by
            // activate_window and hwnd comes from EnumWindows.
            unsafe { visit_activation_window(hwnd, lparam) }
        })
        .unwrap_or(1)
    }

    unsafe fn visit_activation_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let Some(title) = read_window_title(hwnd) else {
            return 1;
        };
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = if pid == 0 {
            String::new()
        } else {
            read_process_name(pid).unwrap_or_else(|| pid.to_string())
        };
        let context = &mut *(lparam as *mut ActivationContext);
        if window_matches(&title, &process, &context.title, &context.process) {
            context.hwnd = Some(hwnd);
            return 0;
        }
        1
    }

    unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        // A panic must never cross the Win32 callback boundary. Treat an
        // unexpected per-window failure like the Python callback does: skip
        // that window and keep enumerating.
        std::panic::catch_unwind(|| visit_window(hwnd, lparam)).unwrap_or(1)
    }

    fn visit_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `hwnd` comes from EnumWindows and `lparam` is the live Vec
        // pointer supplied by list_visible_windows for this synchronous call.
        unsafe { visit_window_ffi(hwnd, lparam) }
    }

    unsafe fn visit_window_ffi(hwnd: HWND, lparam: LPARAM) -> i32 {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let Some(title) = read_window_title(hwnd) else {
            return 1;
        };
        if title.trim().is_empty() {
            return 1;
        }

        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = if pid == 0 {
            None
        } else {
            read_process_name(pid)
        };
        if is_self_window(&title, process.as_deref()) {
            return 1;
        }

        let windows = &mut *(lparam as *mut Vec<VisibleWindow>);
        windows.push(VisibleWindow {
            title,
            process: process.unwrap_or_default(),
        });
        1
    }

    unsafe fn read_window_title(hwnd: HWND) -> Option<String> {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return None;
        }
        let capacity = (length as usize).checked_add(1)?;
        let mut buffer = vec![0_u16; capacity];
        let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), capacity as i32);
        if copied <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..copied as usize]))
    }

    unsafe fn read_process_name(pid: u32) -> Option<String> {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }

        // Match the Python buffer size. QueryFullProcessImageNameW updates
        // `size` to the number of UTF-16 code units written (without NUL).
        let mut buffer = vec![0_u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            return Some(process_name_or_pid(pid, None));
        }
        let end = (size as usize).min(buffer.len());
        let image_path = String::from_utf16_lossy(&buffer[..end]);
        Some(process_name_or_pid(pid, Some(&image_path)))
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{normalized_window_text, VisibleWindow};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    pub(super) fn list_visible_windows() -> Result<Vec<VisibleWindow>, String> {
        Err("window listing is only supported on Windows".to_owned())
    }

    pub(super) fn activate_window(
        target_id: Option<&str>,
        title: &str,
        _process: &str,
    ) -> Result<(), String> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none() {
            return Err(
                "target activation is unavailable on pure Wayland; focus the target before retrying"
                    .to_owned(),
            );
        }
        let id = target_id
            .filter(|value| !value.trim().is_empty())
            .map(str::trim)
            .map(str::to_owned)
            .or_else(|| find_window(title))
            .ok_or_else(|| {
                format!(
                    "could not find the previous target window {:?}",
                    title.trim()
                )
            })?;
        let output = run_xdotool(&["windowactivate", "--sync", &id])?;
        if !output.status.success() {
            return Err(format!(
                "xdotool could not activate target window {:?}",
                title.trim()
            ));
        }
        Ok(())
    }

    fn find_window(title: &str) -> Option<String> {
        let pattern = format!("^{}$", regex::escape(title));
        let output = run_xdotool(&["search", "--name", &pattern]).ok()?;
        if !output.status.success() {
            return None;
        }
        for id in String::from_utf8_lossy(&output.stdout).lines() {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            let name = run_xdotool(&["getwindowname", id]).ok()?;
            if name.status.success()
                && normalized_window_text(&String::from_utf8_lossy(&name.stdout))
                    .eq_ignore_ascii_case(&normalized_window_text(title))
            {
                return Some(id.to_owned());
            }
        }
        None
    }

    fn run_xdotool(args: &[&str]) -> Result<std::process::Output, String> {
        let Some(path) = which("xdotool") else {
            return Err("xdotool is required to restore an X11 target window".to_owned());
        };
        let mut command = Command::new(path);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start xdotool: {error}"))?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    return child
                        .wait_with_output()
                        .map_err(|error| format!("could not read xdotool output: {error}"));
                }
                Ok(None) if start.elapsed() < Duration::from_secs(1) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("xdotool timed out while activating the target window".to_owned());
                }
                Err(error) => return Err(format!("xdotool process failed: {error}")),
            }
        }
    }

    fn which(name: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|entry| entry.join(name))
            .find(|candidate| candidate.is_file())
    }
}

#[cfg(test)]
#[path = "window_enumeration_tests.rs"]
mod tests;
