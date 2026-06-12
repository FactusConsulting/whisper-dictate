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
/// installer/zip from here. NOTE: `releases/latest` resolves to the newest
/// FINAL release and EXCLUDES prereleases, so a prerelease offer must instead
/// link the specific tag (see [`release_tag_url`]).
const RELEASES_LATEST_URL: &str =
    "https://github.com/FactusConsulting/whisper-dictate/releases/latest";

/// Build the release page URL for a SPECIFIC version tag, e.g.
/// `https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.10.0-rc.1`.
///
/// Used for prerelease offers because `releases/latest` skips prereleases. The
/// version is normalized to a single leading `v` (the feed may carry it with or
/// without one). Pure / unit-tested.
fn release_tag_url(version: &str) -> String {
    let v = version.trim().trim_start_matches('v');
    format!("https://github.com/FactusConsulting/whisper-dictate/releases/tag/v{v}")
}

/// Classify how this instance was installed from the running executable's path
/// and an explicit Chocolatey package-directory probe.
///
/// Pure: takes the path + OS + a pre-computed directory-existence flag so every
/// branch is unit-testable on any host. The match is intentionally ordered
/// most-specific-first.
///
/// On Windows the priority is:
/// 1. `choco_pkg_dir_exists` → [`InstallMethod::Choco`] (checked **first**
///    because our Chocolatey package is a wrapper around the Inno installer: the
///    exe always lands in `%LOCALAPPDATA%\Programs\WhisperDictate`, so the exe
///    path alone can never distinguish a Chocolatey install from a bare Inno
///    install).
/// 2. `\chocolatey\lib\whisper-dictate\` in the exe path → Choco (legacy
///    signal, kept as a secondary / belt-and-suspenders check — harmless).
/// 3. WinGet / WindowsApps path fragments → [`InstallMethod::Winget`].
/// 4. `\Programs\WhisperDictate` / `Program Files` → [`InstallMethod::Installer`].
/// 5. Anything else → [`InstallMethod::Portable`].
///
/// Non-Windows OSes ignore `choco_pkg_dir_exists` (always `false` there).
pub(in crate::ui) fn detect_install_method(
    exe_path: &str,
    os: Os,
    choco_pkg_dir_exists: bool,
) -> InstallMethod {
    match os {
        Os::Windows => {
            let lower = exe_path.replace('/', "\\").to_ascii_lowercase();
            // Priority 1: Chocolatey package directory exists on disk — the exe
            // path is not a reliable signal because the wrapper uses the Inno
            // installer, which always writes to %LOCALAPPDATA%\Programs\WhisperDictate.
            if choco_pkg_dir_exists {
                InstallMethod::Choco
            } else if lower.contains("\\chocolatey\\lib\\whisper-dictate\\") {
                // Priority 2: exe path contains the chocolatey lib path
                // (fallback / direct-install edge case).
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
///
/// `offered_version` is the version string the badge is offering and
/// `offered_is_prerelease` whether it is an `-rc.N` pre-release. When the offer
/// is a prerelease, the behaviour adapts:
/// - Choco: add `--prerelease` and pin `--version=<offered_version>` so the
///   exact rc is installed (Chocolatey otherwise upgrades to the newest STABLE).
/// - Every other method (winget / nix / brew / installer / portable): the
///   published winget manifest, the flake, and the Homebrew formula track FINAL
///   releases only, and `releases/latest` excludes prereleases — so point the
///   user at the rc's specific tag page to download the matching artifact.
///
/// For a FINAL offer the behaviour is exactly as before (the `offered_version`
/// is unused on the choco final path so existing commands are byte-identical).
pub(in crate::ui) fn upgrade_action(
    method: InstallMethod,
    offered_version: &str,
    offered_is_prerelease: bool,
) -> UpgradeAction {
    // A prerelease offer can only be satisfied by Chocolatey's --prerelease flag
    // or a direct download; package managers that track finals send the user to
    // the rc's tag page instead.
    if offered_is_prerelease {
        return match method {
            InstallMethod::Choco => UpgradeAction::Command(format!(
                "choco upgrade whisper-dictate --source=whisper-dictate --prerelease --version={} -y",
                offered_version.trim().trim_start_matches('v')
            )),
            InstallMethod::Winget
            | InstallMethod::Nix
            | InstallMethod::Brew
            | InstallMethod::Installer
            | InstallMethod::Portable => UpgradeAction::OpenUrl(release_tag_url(offered_version)),
        };
    }

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
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Choco
        );
    }

    #[test]
    fn windows_winget_packages_path_is_winget() {
        let path = r"C:\Users\lars\AppData\Local\Microsoft\WinGet\Packages\FactusConsulting.WhisperDictate_abc\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Winget
        );
    }

    #[test]
    fn windows_windowsapps_path_is_winget() {
        let path = r"C:\Program Files\WindowsApps\FactusConsulting.WhisperDictate_1.9.5_x64\whisper-dictate.exe";
        // WindowsApps is matched before the Program Files installer rule.
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Winget
        );
    }

    #[test]
    fn windows_inno_localappdata_path_is_installer() {
        // The Inno installer's per-user target.
        let path = r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Installer
        );
    }

    #[test]
    fn windows_program_files_path_is_installer() {
        let path = r"C:\Program Files\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Installer
        );
    }

    #[test]
    fn windows_arbitrary_path_is_portable() {
        let path = r"D:\downloads\whisper-dictate-portable\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Portable
        );
    }

    #[test]
    fn windows_detection_is_case_insensitive() {
        let path = r"C:\PROGRAMDATA\Chocolatey\Lib\Whisper-Dictate\tools\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Choco
        );
    }

    #[test]
    fn windows_forward_slashes_are_normalized() {
        let path = "C:/ProgramData/chocolatey/lib/whisper-dictate/tools/whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Choco
        );
    }

    // ── choco_pkg_dir_exists flag (the wrapper-install real-world case) ───────

    #[test]
    fn windows_choco_pkg_dir_present_and_inno_exe_path_is_choco() {
        // Real-world case: Chocolatey wrapper installs via Inno, so the exe ends
        // up in %LOCALAPPDATA%\Programs\WhisperDictate. The path heuristic would
        // classify this as Installer, but choco_pkg_dir_exists=true takes priority.
        let path = r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, true),
            InstallMethod::Choco
        );
    }

    #[test]
    fn windows_choco_pkg_dir_absent_and_inno_exe_path_is_installer() {
        // When the Chocolatey package dir does NOT exist and the exe is in the
        // Inno location, we correctly fall through to Installer.
        let path = r"C:\Users\lars\AppData\Local\Programs\WhisperDictate\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Installer
        );
    }

    #[test]
    fn windows_choco_pkg_dir_absent_and_choco_lib_exe_path_is_choco() {
        // Secondary signal: even without the dir flag, the legacy chocolatey\lib
        // path still maps to Choco (e.g. hypothetical direct-install scenario).
        let path = r"C:\ProgramData\chocolatey\lib\whisper-dictate\tools\whisper-dictate.exe";
        assert_eq!(
            detect_install_method(path, Os::Windows, false),
            InstallMethod::Choco
        );
    }

    #[test]
    fn linux_nix_store_path_is_nix() {
        let path = "/nix/store/abc123-whisper-dictate-1.9.5/bin/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Linux, false),
            InstallMethod::Nix
        );
    }

    #[test]
    fn linux_arbitrary_path_is_portable() {
        let path = "/home/lars/.local/bin/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Linux, false),
            InstallMethod::Portable
        );
    }

    #[test]
    fn macos_cellar_path_is_brew() {
        let path = "/opt/homebrew/Cellar/whisper-dictate/1.9.5/bin/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Mac, false),
            InstallMethod::Brew
        );
    }

    #[test]
    fn macos_arbitrary_path_is_portable() {
        let path = "/Applications/WhisperDictate.app/Contents/MacOS/whisper-dictate";
        assert_eq!(
            detect_install_method(path, Os::Mac, false),
            InstallMethod::Portable
        );
    }

    // ── upgrade_action ───────────────────────────────────────────────────────

    // A FINAL offer: the version + flag are inert on the existing paths, so the
    // commands/URLs are byte-identical to before the prerelease change.
    fn final_action(method: InstallMethod) -> UpgradeAction {
        upgrade_action(method, "1.10.0", false)
    }

    #[test]
    fn choco_maps_to_command_with_feed_source() {
        assert_eq!(
            final_action(InstallMethod::Choco),
            UpgradeAction::Command(
                "choco upgrade whisper-dictate --source=whisper-dictate -y".to_owned()
            )
        );
    }

    #[test]
    fn winget_maps_to_command_with_correct_package_id() {
        // The id must match packaging/windows/winget/*.yaml.
        assert_eq!(
            final_action(InstallMethod::Winget),
            UpgradeAction::Command("winget upgrade FactusConsulting.WhisperDictate".to_owned())
        );
    }

    #[test]
    fn nix_maps_to_flake_upgrade_command() {
        assert_eq!(
            final_action(InstallMethod::Nix),
            UpgradeAction::Command(
                "nix profile upgrade github:FactusConsulting/whisper-dictate".to_owned()
            )
        );
    }

    #[test]
    fn brew_maps_to_upgrade_command() {
        assert_eq!(
            final_action(InstallMethod::Brew),
            UpgradeAction::Command("brew upgrade whisper-dictate".to_owned())
        );
    }

    #[test]
    fn installer_maps_to_release_url() {
        assert_eq!(
            final_action(InstallMethod::Installer),
            UpgradeAction::OpenUrl(
                "https://github.com/FactusConsulting/whisper-dictate/releases/latest".to_owned()
            )
        );
    }

    #[test]
    fn portable_maps_to_release_url() {
        assert_eq!(
            final_action(InstallMethod::Portable),
            UpgradeAction::OpenUrl(
                "https://github.com/FactusConsulting/whisper-dictate/releases/latest".to_owned()
            )
        );
    }

    // ── prerelease offers ─────────────────────────────────────────────────────

    #[test]
    fn choco_prerelease_offer_adds_flag_and_pins_version() {
        assert_eq!(
            upgrade_action(InstallMethod::Choco, "1.10.0-rc.2", true),
            UpgradeAction::Command(
                "choco upgrade whisper-dictate --source=whisper-dictate --prerelease \
                 --version=1.10.0-rc.2 -y"
                    .to_owned()
            )
        );
    }

    #[test]
    fn choco_prerelease_offer_strips_leading_v_in_pin() {
        // A feed entry with a leading `v` must not double up in --version.
        assert_eq!(
            upgrade_action(InstallMethod::Choco, "v1.10.0-rc.1", true),
            UpgradeAction::Command(
                "choco upgrade whisper-dictate --source=whisper-dictate --prerelease \
                 --version=1.10.0-rc.1 -y"
                    .to_owned()
            )
        );
    }

    #[test]
    fn non_choco_prerelease_offer_points_at_tag_url() {
        // winget / nix / brew / installer / portable all track finals or skip
        // prereleases, so a prerelease offer links the specific rc tag.
        let tag = "https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.10.0-rc.2";
        for method in [
            InstallMethod::Winget,
            InstallMethod::Nix,
            InstallMethod::Brew,
            InstallMethod::Installer,
            InstallMethod::Portable,
        ] {
            assert_eq!(
                upgrade_action(method, "1.10.0-rc.2", true),
                UpgradeAction::OpenUrl(tag.to_owned()),
                "method {method:?} should link the rc tag page"
            );
        }
    }

    #[test]
    fn tag_url_normalizes_leading_v() {
        assert_eq!(
            release_tag_url("1.10.0-rc.1"),
            "https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.10.0-rc.1"
        );
        assert_eq!(
            release_tag_url("v1.10.0-rc.1"),
            "https://github.com/FactusConsulting/whisper-dictate/releases/tag/v1.10.0-rc.1"
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
