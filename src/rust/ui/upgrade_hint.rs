//! Install-method detection + per-method upgrade action for the sidebar
//! "update available" badge.
//!
//! All logic here is PURE and unit-tested: given the running executable's path
//! and the host OS, [`detect_install_method`] classifies HOW this instance was
//! installed, and [`upgrade_action`] maps that to the concrete thing the UI
//! offers — a copyable upgrade command, or a release-page URL to download a new
//! installer/zip. The thin, untestable shell (reading `current_exe()`, copying
//! to the clipboard, opening the URL) lives in the sidebar render code.

/// Host operating system, passed in so [`detect_install_method`] stays pure and
/// testable on any platform (the path rules differ per OS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Os {
    Windows,
    Linux,
    Mac,
}

impl Os {
    /// The OS the binary was compiled for. The single place that reads the
    /// `cfg!` flags so the rest of the module is platform-agnostic and unit
    /// tests can exercise every branch regardless of the test host.
    pub(in crate::ui) fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

/// How this instance of whisper-dictate was installed, inferred from the path of
/// the running executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum InstallMethod {
    /// Windows: installed via the Chocolatey package (under `\chocolatey\lib\`).
    Choco,
    /// Windows: installed via winget (under a WinGet packages / WindowsApps path).
    Winget,
    /// Windows: installed via the Inno Setup installer (per-user
    /// `{localappdata}\Programs\WhisperDictate`, or a `Program Files` location).
    Installer,
    /// Linux: installed via the Nix flake (binary lives in `/nix/store/`).
    Nix,
    /// macOS: installed via Homebrew (binary under a Cellar path).
    Brew,
    /// Anything else: a portable unzip / dev build. The user re-downloads.
    Portable,
}

/// The action the badge performs for a given install method.
///
/// A typed enum (rather than a bare string) lets the UI render two distinct
/// affordances: `Command` is copied to the clipboard so the user can paste-and-
/// run it; `OpenUrl` opens the release page so the user downloads a fresh
/// installer/zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum UpgradeAction {
    /// A shell command to copy to the clipboard and run.
    Command(String),
    /// A URL to open (the latest-release page).
    OpenUrl(String),
}

/// The winget package identifier, confirmed from
/// `packaging/windows/winget/FactusConsulting.WhisperDictate.yaml`
/// (`PackageIdentifier: FactusConsulting.WhisperDictate`).
const WINGET_PACKAGE_ID: &str = "FactusConsulting.WhisperDictate";

/// The latest-release page. Installer/portable users download the new
/// installer/zip from here.
const RELEASES_LATEST_URL: &str =
    "https://github.com/FactusConsulting/whisper-dictate/releases/latest";

/// Classify how this instance was installed from the running executable's path.
///
/// Pure: takes the path + OS so every branch is unit-testable on any host. The
/// match is intentionally ordered most-specific-first so e.g. a Chocolatey shim
/// under `Program Files` is still classified as Choco. Matching is
/// case-insensitive on Windows because installed paths there are not
/// case-sensitive.
pub(in crate::ui) fn detect_install_method(exe_path: &str, os: Os) -> InstallMethod {
    match os {
        Os::Windows => {
            let lower = exe_path.replace('/', "\\").to_ascii_lowercase();
            if lower.contains("\\chocolatey\\lib\\whisper-dictate\\") {
                InstallMethod::Choco
            } else if lower.contains("\\microsoft\\winget\\packages\\")
                || lower.contains("\\windowsapps\\")
            {
                InstallMethod::Winget
            } else if lower.contains("\\programs\\whisperdictate")
                || lower.contains("\\program files\\")
                || lower.contains("\\program files (x86)\\")
            {
                InstallMethod::Installer
            } else {
                InstallMethod::Portable
            }
        }
        Os::Linux => {
            if exe_path.starts_with("/nix/store/") {
                InstallMethod::Nix
            } else {
                InstallMethod::Portable
            }
        }
        Os::Mac => {
            // Homebrew installs under a Cellar directory, e.g.
            // /opt/homebrew/Cellar/... (Apple Silicon) or
            // /usr/local/Cellar/... (Intel).
            if exe_path.contains("/Cellar/") {
                InstallMethod::Brew
            } else {
                InstallMethod::Portable
            }
        }
    }
}

/// Map an install method to the concrete upgrade action the badge offers.
///
/// Pure / unit-tested. Command strings are the exact paste-and-run commands;
/// the Choco source name matches the project's feed (`--source=whisper-dictate`)
/// and the winget id matches the published manifest.
pub(in crate::ui) fn upgrade_action(method: InstallMethod) -> UpgradeAction {
    match method {
        InstallMethod::Choco => UpgradeAction::Command(
            "choco upgrade whisper-dictate --source=whisper-dictate -y".to_owned(),
        ),
        InstallMethod::Winget => {
            UpgradeAction::Command(format!("winget upgrade {WINGET_PACKAGE_ID}"))
        }
        // The flake-URL form matches the README's install command
        // (`nix profile install github:FactusConsulting/whisper-dictate`) so the
        // upgrade resolves the same flake reference.
        InstallMethod::Nix => UpgradeAction::Command(
            "nix profile upgrade github:FactusConsulting/whisper-dictate".to_owned(),
        ),
        // The new Homebrew formula is pulled then upgraded.
        InstallMethod::Brew => UpgradeAction::Command("brew upgrade whisper-dictate".to_owned()),
        // No package manager owns this install: send the user to the release page
        // to fetch the new installer/zip.
        InstallMethod::Installer | InstallMethod::Portable => {
            UpgradeAction::OpenUrl(RELEASES_LATEST_URL.to_owned())
        }
    }
}

impl UpgradeAction {
    /// The inner string the click acts on — the command to copy, or the URL to
    /// open. Used by the badge's hover text so the user sees the exact target.
    pub(in crate::ui) fn target(&self) -> &str {
        match self {
            UpgradeAction::Command(cmd) => cmd,
            UpgradeAction::OpenUrl(url) => url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_install_method ────────────────────────────────────────────────

    #[test]
    fn windows_choco_lib_path_is_choco() {
        let path = r"C:\ProgramData\chocolatey\lib\whisper-dictate\tools\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Choco
        );
    }

    #[test]
    fn windows_winget_packages_path_is_winget() {
        let path = r"C:\Users\lars\AppData\Local\Microsoft\WinGet\Packages\FactusConsulting.WhisperDictate_abc\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Winget
        );
    }

    #[test]
    fn windows_windowsapps_path_is_winget() {
        let path = r"C:\Program Files\WindowsApps\FactusConsulting.WhisperDictate_1.9.5_x64\whisper-dictate.exe";
        // WindowsApps is matched before the Program Files installer rule.
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Winget
        );
    }

    #[test]
    fn windows_inno_localappdata_path_is_installer() {
        // The Inno installer's per-user target.
        let path = r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Installer
        );
    }

    #[test]
    fn windows_program_files_path_is_installer() {
        let path = r"C:\Program Files\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Installer
        );
    }

    #[test]
    fn windows_arbitrary_path_is_portable() {
        let path = r"D:\downloads\whisper-dictate-portable\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Portable
        );
    }

    #[test]
    fn windows_detection_is_case_insensitive() {
        let path = r"C:\PROGRAMDATA\Chocolatey\Lib\Whisper-Dictate\tools\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Choco
        );
    }

    #[test]
    fn windows_forward_slashes_are_normalized() {
        let path = "C:/ProgramData/chocolatey/lib/whisper-dictate/tools/whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows),
            InstallMethod::Choco
        );
    }

    #[test]
    fn linux_nix_store_path_is_nix() {
        let path = "/nix/store/abc123-whisper-dictate-1.9.5/bin/whisper-dictate";
        assert_eq!(detect_install_method(path, Os::Linux), InstallMethod::Nix);
    }

    #[test]
    fn linux_arbitrary_path_is_portable() {
        let path = "/home/lars/.local/bin/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Linux),
            InstallMethod::Portable
        );
    }

    #[test]
    fn macos_cellar_path_is_brew() {
        let path = "/opt/homebrew/Cellar/whisper-dictate/1.9.5/bin/whisper-dictate";
        assert_eq!(detect_install_method(path, Os::Mac), InstallMethod::Brew);
    }

    #[test]
    fn macos_arbitrary_path_is_portable() {
        let path = "/Applications/WhisperDictate.app/Contents/MacOS/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Mac),
            InstallMethod::Portable
        );
    }

    // ── upgrade_action ───────────────────────────────────────────────────────

    #[test]
    fn choco_maps_to_command_with_feed_source() {
        assert_eq!(
            upgrade_action(InstallMethod::Choco),
            UpgradeAction::Command(
                "choco upgrade whisper-dictate --source=whisper-dictate -y".to_owned()
            )
        );
    }

    #[test]
    fn winget_maps_to_command_with_correct_package_id() {
        // The id must match packaging/windows/winget/*.yaml.
        assert_eq!(
            upgrade_action(InstallMethod::Winget),
            UpgradeAction::Command("winget upgrade FactusConsulting.WhisperDictate".to_owned())
        );
    }

    #[test]
    fn nix_maps_to_flake_upgrade_command() {
        assert_eq!(
            upgrade_action(InstallMethod::Nix),
            UpgradeAction::Command(
                "nix profile upgrade github:FactusConsulting/whisper-dictate".to_owned()
            )
        );
    }

    #[test]
    fn brew_maps_to_upgrade_command() {
        assert_eq!(
            upgrade_action(InstallMethod::Brew),
            UpgradeAction::Command("brew upgrade whisper-dictate".to_owned())
        );
    }

    #[test]
    fn installer_maps_to_release_url() {
        assert_eq!(
            upgrade_action(InstallMethod::Installer),
            UpgradeAction::OpenUrl(
                "https://github.com/FactusConsulting/whisper-dictate/releases/latest".to_owned()
            )
        );
    }

    #[test]
    fn portable_maps_to_release_url() {
        assert_eq!(
            upgrade_action(InstallMethod::Portable),
            UpgradeAction::OpenUrl(
                "https://github.com/FactusConsulting/whisper-dictate/releases/latest".to_owned()
            )
        );
    }

    #[test]
    fn target_returns_inner_string_for_both_variants() {
        assert_eq!(
            UpgradeAction::Command("winget upgrade x".to_owned()).target(),
            "winget upgrade x"
        );
        assert_eq!(
            UpgradeAction::OpenUrl("https://example/latest".to_owned()).target(),
            "https://example/latest"
        );
    }
}
