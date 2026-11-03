//! Starting the processes behind the Windows one-click update: the
//! package-manager console (elevated for Chocolatey) and the downloaded setup
//! wizard.
//!
//! Request building is platform-neutral and unit-tested; only the final
//! `ShellExecuteW` / process spawn is Windows-specific. On other platforms
//! the launchers return an error, and `upgrade_hint` never selects a
//! Windows-only action there.

use super::update_install::{powershell_command_args, windows_command_line, ConsoleUpdate};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Windows PowerShell 5.1 under `system_root` (the `SystemRoot` environment
/// variable, `C:\Windows` when unset). Absolute, so neither the elevated nor
/// the plain launch resolves `powershell.exe` through the current directory
/// or a user-writable `PATH` entry.
pub(in crate::ui) fn windows_powershell_path(system_root: Option<&OsStr>) -> PathBuf {
    Path::new(system_root.unwrap_or(OsStr::new(r"C:\Windows")))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

/// One `ShellExecuteW` call. Only Windows executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(in crate::ui) struct ShellExecuteRequest {
    pub(in crate::ui) verb: &'static str,
    pub(in crate::ui) file: PathBuf,
    pub(in crate::ui) parameters: String,
}

/// The elevated console launch: verb `runas` shows the UAC prompt and opens
/// `powershell` in a visible console running `script`.
pub(in crate::ui) fn elevated_console_request(
    powershell: PathBuf,
    script: &str,
) -> ShellExecuteRequest {
    ShellExecuteRequest {
        verb: "runas",
        file: powershell,
        parameters: windows_command_line(&powershell_command_args(script)),
    }
}

/// Start `update` in a visible PowerShell console. `relaunch_exe` is the
/// running `wd-gui.exe`, started again after a successful update.
pub(in crate::ui) fn start_console_update(
    update: &ConsoleUpdate,
    relaunch_exe: &Path,
) -> Result<(), String> {
    let script = update.script(&relaunch_exe.to_string_lossy());
    let powershell = windows_powershell_path(std::env::var_os("SystemRoot").as_deref());
    if update.elevated() {
        shell_execute(&elevated_console_request(powershell, &script))
    } else {
        spawn_console(&powershell, &powershell_command_args(&script))
    }
}

/// Run the elevated request. An elevated process does not inherit this
/// process's environment, so no credentials travel with it.
#[cfg(windows)]
fn shell_execute(request: &ShellExecuteRequest) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
    let verb = wide(OsStr::new(request.verb));
    let file = wide(request.file.as_os_str());
    let parameters = wide(OsStr::new(&request.parameters));
    // SAFETY: every pointer is a NUL-terminated UTF-16 buffer that outlives
    // the call; a null HWND and directory are documented defaults.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success as a value greater than 32.
    if result as isize > 32 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // ERROR_CANCELLED: the user declined the UAC prompt.
    if err.raw_os_error() == Some(1223) {
        Err("the administrator prompt was cancelled".to_owned())
    } else {
        Err(format!(
            "could not start PowerShell as administrator: {err}"
        ))
    }
}

#[cfg(not(windows))]
fn shell_execute(_request: &ShellExecuteRequest) -> Result<(), String> {
    Err(WINDOWS_ONLY.to_owned())
}

/// Spawn `powershell` with `args` in a new visible console, without the
/// session's credentials in its environment.
#[cfg(windows)]
fn spawn_console(powershell: &Path, args: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    let mut command = std::process::Command::new(powershell);
    command.args(args).creation_flags(CREATE_NEW_CONSOLE);
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    command
        .spawn()
        .map(drop)
        .map_err(|err| format!("could not start PowerShell: {err}"))
}

#[cfg(not(windows))]
fn spawn_console(_powershell: &Path, _args: &[String]) -> Result<(), String> {
    Err(WINDOWS_ONLY.to_owned())
}

/// Start the downloaded setup wizard (interactive, no silent flags). The
/// installer is per-user, so it needs no elevation; it closes the running
/// app itself and offers to start the new version when it finishes.
#[cfg(windows)]
pub(in crate::ui) fn start_installer(installer: &Path) -> Result<(), String> {
    let mut command = std::process::Command::new(installer);
    if let Some(dir) = installer.parent() {
        command.current_dir(dir);
    }
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    command
        .spawn()
        .map(drop)
        .map_err(|err| format!("could not start {}: {err}", installer.display()))
}

#[cfg(not(windows))]
pub(in crate::ui) fn start_installer(_installer: &Path) -> Result<(), String> {
    Err(WINDOWS_ONLY.to_owned())
}

#[cfg(not(windows))]
const WINDOWS_ONLY: &str = "one-click updates are only available on Windows";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_path_is_absolute_under_system_root() {
        assert_eq!(
            windows_powershell_path(Some(OsStr::new(r"D:\Win"))),
            Path::new(r"D:\Win")
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        );
        assert!(windows_powershell_path(None).starts_with(r"C:\Windows"));
    }

    #[test]
    fn elevated_request_runs_the_script_with_runas() {
        let script = "Write-Host 'it''s'; exit 0";
        let request = elevated_console_request(PathBuf::from("powershell.exe"), script);
        assert_eq!(request.verb, "runas");
        assert_eq!(request.file, PathBuf::from("powershell.exe"));
        assert_eq!(
            request.parameters,
            "-NoProfile -ExecutionPolicy Bypass -Command \"Write-Host 'it''s'; exit 0\""
        );
    }

    /// Hand Windows PowerShell the exact command line the elevated launch
    /// uses, but non-elevated and with a probe that only PARSES the real
    /// Chocolatey script (nothing runs) and prints the `$appExe` literal. This
    /// proves the command-line quoting and the PowerShell quoting round-trip a
    /// path with a space, an apostrophe, a typographic quote and a non-ASCII
    /// letter.
    #[cfg(windows)]
    #[test]
    fn windows_powershell_receives_the_script_intact() {
        use super::super::update_install::powershell_literal;
        use std::os::windows::process::CommandExt;

        let exe = "C:\\Users\\Anne O'Brien \u{2019}S\u{f8}ren\\WhisperDictate\\wd-gui.exe";
        let script = ConsoleUpdate::Choco {
            version: "3.6.0-rc.1".to_owned(),
            prerelease: true,
        }
        .script(exe);
        let probe = format!(
            "$errs = $null; \
             $ast = [System.Management.Automation.Language.Parser]::ParseInput({}, [ref]$null, [ref]$errs); \
             if ($errs.Count -gt 0) {{ $errs | ForEach-Object {{ Write-Output $_.Message }}; exit 1 }}; \
             $assign = $ast.FindAll({{ param($n) $n -is [System.Management.Automation.Language.AssignmentStatementAst] }}, $true) | \
             Where-Object {{ $_.Left.VariablePath.UserPath -eq 'appExe' }} | Select-Object -First 1; \
             Write-Output (($assign.Right.Expression.Value.ToCharArray() | ForEach-Object {{ [int]$_ }}) -join ' ')",
            powershell_literal(&script)
        );
        let request = elevated_console_request(
            windows_powershell_path(std::env::var_os("SystemRoot").as_deref()),
            &probe,
        );
        let output = std::process::Command::new(&request.file)
            .raw_arg(&request.parameters)
            .output()
            .expect("run powershell.exe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "parse failed: {stdout}");
        let expected = exe
            .encode_utf16()
            .map(|unit| unit.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(stdout.trim(), expected);
    }
}
