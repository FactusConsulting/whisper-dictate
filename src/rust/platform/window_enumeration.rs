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
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    use super::{is_self_window, process_name_or_pid, VisibleWindow};

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
    use super::VisibleWindow;

    pub(super) fn list_visible_windows() -> Result<Vec<VisibleWindow>, String> {
        Err("window listing is only supported on Windows".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_cli_and_gui_process_names_case_insensitively() {
        assert!(is_self_window("Settings", Some("whisper-dictate.exe")));
        assert!(is_self_window(
            "Settings",
            Some(r"C:\Apps\Whisper-Dictate-GUI.EXE")
        ));
        assert!(is_self_window("Settings", Some("whisper_dictate")));
        assert!(!is_self_window("Settings", Some("notepad.exe")));
        assert!(!is_self_window("Settings", None));
    }

    #[test]
    fn filters_self_titles_with_optional_numeric_version() {
        assert!(is_self_window("Whisper-Dictate", None));
        assert!(is_self_window("  WHISPER-DICTATE   1.22.4  ", None));
        assert!(!is_self_window("Whisper-Dictate Settings", None));
        assert!(!is_self_window("Whisper-Dictate beta", None));
    }

    #[test]
    fn process_name_uses_windows_basename_when_query_succeeds() {
        assert_eq!(
            process_name_or_pid(42, Some(r"C:\Program Files\Notepad\notepad.exe")),
            "notepad.exe"
        );
        assert_eq!(
            process_name_or_pid(42, Some("C:/Tools/Code.exe")),
            "Code.exe"
        );
    }

    #[test]
    fn process_name_falls_back_to_pid_when_image_query_fails() {
        assert_eq!(process_name_or_pid(4242, None), "4242");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn enumeration_reports_unsupported_platform() {
        assert_eq!(
            list_visible_windows().unwrap_err(),
            "window listing is only supported on Windows"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn live_enumeration_never_returns_blank_or_self_windows() {
        for window in list_visible_windows().unwrap() {
            assert!(!window.title.trim().is_empty());
            assert!(!is_self_window(&window.title, Some(&window.process)));
        }
    }
}
