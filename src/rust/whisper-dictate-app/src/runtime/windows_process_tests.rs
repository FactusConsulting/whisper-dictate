use super::*;

#[cfg(windows)]
#[test]
fn stale_process_cleanup_script_is_scoped_to_current_exe_and_app_root() {
    let script = stale_process_cleanup_script(
        123,
        Path::new(r"C:\Program Files\WhisperDictate\whisper-dictate.exe"),
        Path::new(r"C:\Program Files\WhisperDictate"),
    );

    assert!(script.contains("$currentPid = 123"));
    assert!(script.contains("$cleanupPid = $PID"));
    assert!(script.contains(r"$_.ExecutablePath -eq $exe"));
    assert!(script.contains("$_.ProcessId -ne $cleanupPid"));
    assert!(script.contains(r#"$_.CommandLine -like "*whisper_dictate.runtime*""#));
    assert!(script.contains(r#"$_.CommandLine -like "*$root*""#));
    assert!(!script.contains("Stop-Process -Name python"));
    assert!(!script.contains("taskkill /IM python"));
}

#[cfg(windows)]
#[test]
fn powershell_single_quote_escape_doubles_quotes() {
    assert_eq!(
        escape_powershell_single_quoted(r"C:\It's\app"),
        r"C:\It''s\app"
    );
}

#[cfg(windows)]
#[test]
fn windows_shell_prefers_pwsh_when_present_on_path() {
    if env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|dir| dir.join("pwsh.exe").exists()))
        .unwrap_or(false)
    {
        assert_eq!(windows_shell_program(), "pwsh.exe");
    }
}
