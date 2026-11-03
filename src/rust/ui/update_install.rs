//! Pure building blocks for the Windows one-click update behind the sidebar
//! "vX.Y.Z available" badge.
//!
//! - [`ConsoleUpdate`] describes a package-manager update (Chocolatey or
//!   winget) and builds the one-line PowerShell script its console runs.
//! - [`powershell_command_args`] and [`windows_command_line`] turn that script
//!   into a `powershell.exe -Command` command line.
//! - [`release_asset_url`], [`installer_asset_name`] and [`expected_sha256`]
//!   locate and verify the setup executable for the plain-installer path.
//!
//! Process launching lives in `update_install_launch`; the download worker and
//! the badge's flow state live in `update_install_flow`.

use std::fmt;
use std::path::{Path, PathBuf};

/// The winget package identifier, matching
/// `packaging/windows/winget/FactusConsulting.WhisperDictate.yaml`
/// (`PackageIdentifier: FactusConsulting.WhisperDictate`).
pub(in crate::ui) const WINGET_PACKAGE_ID: &str = "FactusConsulting.WhisperDictate";

/// Base of every GitHub release asset download URL; the tag and asset name
/// follow.
const RELEASE_DOWNLOAD_BASE: &str =
    "https://github.com/FactusConsulting/whisper-dictate/releases/download";

/// The checksum manifest published with every release.
pub(in crate::ui) const SHA256SUMS_ASSET: &str = "sha256sums.txt";

/// A version string from the feed without surrounding whitespace or a leading
/// `v`. The feed's versions are already parsed to `X.Y.Z` / `X.Y.Z-rc.N` by
/// `update_check`, so the result is safe in a URL path or file name.
pub(in crate::ui) fn normalize_version(raw: &str) -> &str {
    raw.trim().trim_start_matches('v')
}

/// An update the app runs itself after the user confirms (Windows only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum UpdateRun {
    /// Run a package manager in a visible PowerShell console.
    Console(ConsoleUpdate),
    /// Download the setup executable for `version`, verify its SHA-256
    /// against the release's `sha256sums.txt`, and open the setup wizard.
    Installer { version: String },
}

impl UpdateRun {
    /// What the run executes or fetches, shown in the badge hover.
    pub(in crate::ui) fn target(&self) -> String {
        match self {
            UpdateRun::Console(update) => update.command_line(),
            UpdateRun::Installer { version } => {
                release_asset_url(version, &installer_asset_name(version))
            }
        }
    }
}

/// A package-manager update run in a visible PowerShell console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ConsoleUpdate {
    /// `choco upgrade` pinned to `version` from the project's feed. Runs
    /// elevated because Chocolatey's package directory is machine-wide.
    /// `prerelease` adds `--prerelease`, which Chocolatey needs to resolve an
    /// `-rc.N` version.
    Choco { version: String, prerelease: bool },
    /// `winget upgrade` of the published package. Runs non-elevated; winget
    /// requests elevation itself when an installer needs it.
    Winget,
}

impl ConsoleUpdate {
    /// The executable the console runs.
    pub(in crate::ui) fn tool(&self) -> &'static str {
        match self {
            ConsoleUpdate::Choco { .. } => "choco",
            ConsoleUpdate::Winget => "winget",
        }
    }

    /// The arguments passed to [`Self::tool`].
    pub(in crate::ui) fn args(&self) -> Vec<String> {
        match self {
            ConsoleUpdate::Choco {
                version,
                prerelease,
            } => {
                let mut args = vec![
                    "upgrade".to_owned(),
                    "whisper-dictate".to_owned(),
                    "--source=whisper-dictate".to_owned(),
                ];
                if *prerelease {
                    args.push("--prerelease".to_owned());
                }
                args.push(format!("--version={}", normalize_version(version)));
                args.push("-y".to_owned());
                args
            }
            ConsoleUpdate::Winget => vec![
                "upgrade".to_owned(),
                "--id".to_owned(),
                WINGET_PACKAGE_ID.to_owned(),
                "--exact".to_owned(),
            ],
        }
    }

    /// The command as the user would type it, shown in the badge hover and
    /// echoed in the console.
    pub(in crate::ui) fn command_line(&self) -> String {
        format!("{} {}", self.tool(), self.args().join(" "))
    }

    /// Whether the console must start elevated (UAC prompt).
    pub(in crate::ui) fn elevated(&self) -> bool {
        matches!(self, ConsoleUpdate::Choco { .. })
    }

    /// Exit codes that mean the update installed. Chocolatey reports a
    /// pending reboot as 1641 / 3010; the app still restarts in that case.
    fn success_exit_codes(&self) -> &'static [i32] {
        match self {
            ConsoleUpdate::Choco { .. } => &[0, 1641, 3010],
            ConsoleUpdate::Winget => &[0],
        }
    }

    fn intro(&self) -> String {
        match self {
            ConsoleUpdate::Choco { version, .. } => format!(
                "Updating whisper-dictate to {} with Chocolatey.",
                normalize_version(version)
            ),
            ConsoleUpdate::Winget => "Updating whisper-dictate with winget.".to_owned(),
        }
    }

    /// The PowerShell script the update console runs, as one line of
    /// `;`-separated statements for `powershell.exe -Command`.
    ///
    /// It runs the package manager; on a success exit code it starts
    /// `relaunch_exe` through `explorer.exe`, which hands the launch to the
    /// signed-in user's shell so the app comes back non-elevated even when
    /// the console itself is elevated. On failure it prints the exit code and
    /// waits for Enter so the message stays readable.
    ///
    /// `relaunch_exe` is the absolute path of the running `wd-gui.exe`: an
    /// elevated session can resolve `%LOCALAPPDATA%` for a different account.
    /// Every interpolated value is a single-quoted PowerShell literal (see
    /// [`powershell_literal`]); all fixed text is ASCII and contains no double
    /// quote, so the script needs no escaping inside a quoted command-line
    /// argument.
    pub(in crate::ui) fn script(&self, relaunch_exe: &str) -> String {
        let args = self
            .args()
            .iter()
            .map(|arg| powershell_literal(arg))
            .collect::<Vec<_>>()
            .join(", ");
        let ok_codes = self
            .success_exit_codes()
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        [
            "$Host.UI.RawUI.WindowTitle = 'whisper-dictate update'".to_owned(),
            format!("$appExe = {}", powershell_literal(relaunch_exe)),
            format!("$updateArgs = @({args})"),
            format!("$okCodes = @({ok_codes})"),
            format!("Write-Host {}", powershell_literal(&self.intro())),
            "Write-Host 'whisper-dictate closes during the update and starts again when it finishes.'"
                .to_owned(),
            format!(
                "Write-Host {}",
                powershell_literal(&format!("> {}", self.command_line()))
            ),
            "$code = $null".to_owned(),
            format!(
                "try {{ & {} @updateArgs; $code = $LASTEXITCODE }} catch {{ Write-Host $_.Exception.Message }}",
                powershell_literal(self.tool())
            ),
            "if ($okCodes -contains $code) { \
             Write-Host 'Update finished. Starting whisper-dictate...'; \
             Start-Process -FilePath (Join-Path $env:SystemRoot 'explorer.exe') \
             -ArgumentList ([char]34 + $appExe + [char]34); \
             exit 0 }"
                .to_owned(),
            "Write-Host ('Update failed with exit code ' + $code + '.')".to_owned(),
            "Read-Host 'Press Enter to close this window'".to_owned(),
            "exit 1".to_owned(),
        ]
        .join("; ")
    }
}

/// Quote `value` as a single-quoted PowerShell string literal.
///
/// Inside single quotes PowerShell expands nothing (`$`, backtick and `"` are
/// literal); the only escape is doubling the quote character. PowerShell also
/// treats the typographic quotes U+2018..U+201B as single quotes, so those are
/// doubled too.
pub(in crate::ui) fn powershell_literal(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(ch);
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

/// Arguments that make `powershell.exe` run `script` in its console window
/// without loading the user's profile or applying the execution policy.
///
/// The script travels as readable `-Command` text rather than
/// `-EncodedCommand`: Microsoft Defender's machine-learning detection blocks
/// `powershell.exe` launches that carry a long base64 command line.
pub(in crate::ui) fn powershell_command_args(script: &str) -> Vec<String> {
    vec![
        "-NoProfile".to_owned(),
        "-ExecutionPolicy".to_owned(),
        "Bypass".to_owned(),
        "-Command".to_owned(),
        script.to_owned(),
    ]
}

/// Join `args` into a Windows command line that `CommandLineToArgvW` (and the
/// .NET runtime behind `powershell.exe`) splits back into exactly `args`.
///
/// An argument with whitespace or a double quote is wrapped in double quotes;
/// inside, a double quote is escaped as `\"` and the backslashes before it or
/// before the closing quote are doubled. Other backslashes stay literal.
pub(in crate::ui) fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// File name of the Windows setup executable published for `version`.
pub(in crate::ui) fn installer_asset_name(version: &str) -> String {
    format!(
        "whisper-dictate-windows-setup-{}.exe",
        normalize_version(version)
    )
}

/// Download URL of `asset` attached to the `v<version>` release.
pub(in crate::ui) fn release_asset_url(version: &str, asset: &str) -> String {
    format!(
        "{RELEASE_DOWNLOAD_BASE}/v{}/{asset}",
        normalize_version(version)
    )
}

/// Per-version directory under `temp_root` that receives the downloaded
/// installer.
pub(in crate::ui) fn installer_download_dir(temp_root: &Path, version: &str) -> PathBuf {
    temp_root.join(format!(
        "whisper-dictate-update-{}",
        normalize_version(version)
    ))
}

/// Why `sha256sums.txt` could not vouch for an asset. Each case refuses the
/// install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ChecksumError {
    /// No line names the asset.
    Missing(String),
    /// The asset's line does not carry a 64-digit hex SHA-256.
    Malformed(String),
    /// The asset is listed more than once with different hashes.
    Conflicting(String),
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChecksumError::Missing(asset) => {
                write!(f, "{SHA256SUMS_ASSET} has no entry for {asset}")
            }
            ChecksumError::Malformed(asset) => {
                write!(f, "{SHA256SUMS_ASSET} has a malformed hash for {asset}")
            }
            ChecksumError::Conflicting(asset) => {
                write!(f, "{SHA256SUMS_ASSET} lists {asset} with different hashes")
            }
        }
    }
}

/// The lowercase SHA-256 that `sums` (a `sha256sums.txt` body) records for
/// `asset`.
///
/// Accepts the release workflow's `<file>=<sha256>` lines and the coreutils
/// `<sha256>  <file>` / `<sha256> *<file>` shape. Blank lines and `#`
/// comments are skipped; file names match exactly.
pub(in crate::ui) fn expected_sha256(sums: &str, asset: &str) -> Result<String, ChecksumError> {
    let mut found: Option<String> = None;
    for (name, hash) in sums.lines().filter_map(parse_sums_line) {
        if name != asset {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ChecksumError::Malformed(asset.to_owned()));
        }
        let hash = hash.to_ascii_lowercase();
        match &found {
            Some(previous) if *previous != hash => {
                return Err(ChecksumError::Conflicting(asset.to_owned()));
            }
            _ => found = Some(hash),
        }
    }
    found.ok_or_else(|| ChecksumError::Missing(asset.to_owned()))
}

/// Split one `sha256sums.txt` line into `(file name, hash)`.
fn parse_sums_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    if let Some((name, hash)) = line.split_once('=') {
        return Some((name.trim(), hash.trim()));
    }
    let (hash, name) = line.split_once(char::is_whitespace)?;
    let name = name.trim_start();
    Some((name.strip_prefix('*').unwrap_or(name), hash))
}

#[cfg(test)]
#[path = "update_install_tests.rs"]
mod tests;
