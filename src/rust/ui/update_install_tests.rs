//! Tests for the pure one-click update pieces: package-manager arguments, the
//! console script and its quoting, `-EncodedCommand` encoding, release asset
//! URLs, and `sha256sums.txt` parsing.

use super::*;

/// An install path with a space and an apostrophe, the two characters that
/// break naive quoting.
const TRICKY_EXE: &str = r"C:\Users\Anne O'Brien\AppData\Local\Programs\WhisperDictate\wd-gui.exe";

fn choco(version: &str, prerelease: bool) -> ConsoleUpdate {
    ConsoleUpdate::Choco {
        version: version.to_owned(),
        prerelease,
    }
}

// ── package-manager arguments ───────────────────────────────────────────────

#[test]
fn choco_final_args_pin_the_version_from_the_feed() {
    assert_eq!(
        choco("3.6.0", false).args(),
        [
            "upgrade",
            "whisper-dictate",
            "--source=whisper-dictate",
            "--version=3.6.0",
            "-y"
        ]
    );
}

#[test]
fn choco_prerelease_args_add_prerelease_and_strip_leading_v() {
    assert_eq!(
        choco("v3.6.0-rc.1", true).command_line(),
        "choco upgrade whisper-dictate --source=whisper-dictate --prerelease --version=3.6.0-rc.1 -y"
    );
}

#[test]
fn winget_args_target_the_exact_published_id() {
    assert_eq!(
        ConsoleUpdate::Winget.command_line(),
        "winget upgrade --id FactusConsulting.WhisperDictate --exact"
    );
}

#[test]
fn only_choco_runs_elevated() {
    assert!(choco("3.6.0", false).elevated());
    assert!(!ConsoleUpdate::Winget.elevated());
}

// ── PowerShell literal quoting ──────────────────────────────────────────────

#[test]
fn powershell_literal_doubles_apostrophes_and_keeps_everything_else() {
    assert_eq!(powershell_literal("plain"), "'plain'");
    assert_eq!(powershell_literal("O'Brien"), "'O''Brien'");
    // `$`, backtick and double quotes are inert inside single quotes.
    assert_eq!(powershell_literal("a $b `c \"d\""), "'a $b `c \"d\"'");
    assert_eq!(powershell_literal(""), "''");
}

#[test]
fn powershell_literal_doubles_typographic_single_quotes() {
    // PowerShell ends a single-quoted string at any of U+2018..U+201B too.
    for quote in ['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
        assert_eq!(
            powershell_literal(&format!("a{quote}b")),
            format!("'a{quote}{quote}b'")
        );
    }
}

// ── console script ──────────────────────────────────────────────────────────

#[test]
fn choco_script_runs_the_pinned_upgrade_and_relaunches_the_given_exe() {
    let script = choco("3.6.0-rc.2", true).script(TRICKY_EXE);
    let statements = [
        r"$appExe = 'C:\Users\Anne O''Brien\AppData\Local\Programs\WhisperDictate\wd-gui.exe'",
        "$updateArgs = @('upgrade', 'whisper-dictate', '--source=whisper-dictate', \
         '--prerelease', '--version=3.6.0-rc.2', '-y')",
        "$okCodes = @(0, 1641, 3010)",
        "try { & 'choco' @updateArgs; $code = $LASTEXITCODE } catch { Write-Host $_.Exception.Message }",
        // Success: relaunch through explorer.exe so the app is not elevated.
        "if ($okCodes -contains $code) { Write-Host 'Update finished. Starting whisper-dictate...'; \
         Start-Process -FilePath (Join-Path $env:SystemRoot 'explorer.exe') \
         -ArgumentList ([char]34 + $appExe + [char]34); exit 0 }",
        // Failure: show the exit code and wait for Enter.
        "Write-Host ('Update failed with exit code ' + $code + '.')",
        "Read-Host 'Press Enter to close this window'",
        "exit 1",
    ];
    // Each statement is present, in this order.
    let mut from = 0;
    for statement in statements {
        let at = script[from..]
            .find(statement)
            .unwrap_or_else(|| panic!("missing or out of order: {statement}\nin: {script}"));
        from += at + statement.len();
    }
    assert!(script.ends_with("exit 1"));
}

#[test]
fn winget_script_runs_winget_and_only_zero_counts_as_success() {
    let script = ConsoleUpdate::Winget.script(TRICKY_EXE);
    assert!(script.contains(
        "$updateArgs = @('upgrade', '--id', 'FactusConsulting.WhisperDictate', '--exact'); "
    ));
    assert!(script.contains("$okCodes = @(0); "));
    assert!(script.contains("& 'winget' @updateArgs"));
}

#[test]
fn script_is_one_ascii_line_without_double_quotes() {
    for update in [choco("3.6.0", false), ConsoleUpdate::Winget] {
        let script = update.script(TRICKY_EXE);
        assert!(script.is_ascii(), "non-ASCII in script: {script}");
        assert!(!script.contains(['"', '\r', '\n']), "{script}");
    }
}

#[test]
fn script_keeps_a_non_ascii_install_path_verbatim() {
    let exe = r"C:\Users\Søren\AppData\Local\Programs\WhisperDictate\wd-gui.exe";
    assert!(choco("3.6.0", false)
        .script(exe)
        .contains(&format!("$appExe = '{exe}'")));
}

// ── command line ────────────────────────────────────────────────────────────

#[test]
fn powershell_args_bypass_profile_and_policy() {
    assert_eq!(
        powershell_command_args("exit 0"),
        [
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "exit 0"
        ]
    );
}

#[test]
fn command_line_quotes_only_arguments_that_need_it() {
    let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        windows_command_line(&args(&["-NoProfile", "x"])),
        "-NoProfile x"
    );
    assert_eq!(
        windows_command_line(&args(&["-Command", "Write-Host 'O''Brien'; exit 0"])),
        "-Command \"Write-Host 'O''Brien'; exit 0\""
    );
    assert_eq!(windows_command_line(&args(&[""])), "\"\"");
}

#[test]
fn command_line_escapes_quotes_and_trailing_backslashes() {
    let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    // Backslashes stay literal unless they precede a quote.
    assert_eq!(
        windows_command_line(&args(&[r"C:\Program Files\x"])),
        r#""C:\Program Files\x""#
    );
    assert_eq!(windows_command_line(&args(&[r"a dir\"])), r#""a dir\\""#);
    assert_eq!(
        windows_command_line(&args(&[r#"say "hi" \"#])),
        r#""say \"hi\" \\""#
    );
    assert_eq!(windows_command_line(&args(&[r#"a\"b"#])), r#""a\\\"b""#);
}

// ── release assets ──────────────────────────────────────────────────────────

#[test]
fn installer_asset_url_matches_the_release_layout() {
    assert_eq!(
        installer_asset_name("v3.5.2"),
        "whisper-dictate-windows-setup-3.5.2.exe"
    );
    assert_eq!(
        release_asset_url("3.5.2", &installer_asset_name("3.5.2")),
        "https://github.com/FactusConsulting/whisper-dictate/releases/download/v3.5.2/\
         whisper-dictate-windows-setup-3.5.2.exe"
    );
    assert_eq!(
        release_asset_url(" v3.6.0-rc.1 ", SHA256SUMS_ASSET),
        "https://github.com/FactusConsulting/whisper-dictate/releases/download/v3.6.0-rc.1/\
         sha256sums.txt"
    );
}

#[test]
fn installer_download_dir_is_per_version_under_the_temp_root() {
    assert_eq!(
        installer_download_dir(Path::new("tmp"), "v3.6.0"),
        Path::new("tmp").join("whisper-dictate-update-3.6.0")
    );
}

// ── sha256sums.txt ──────────────────────────────────────────────────────────

const SETUP: &str = "whisper-dictate-windows-setup-3.5.2.exe";
const SETUP_HASH: &str = "4b0c7c4bd8b0d1bbf6a3f0d4bb9e0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f70";

#[test]
fn sha256sums_release_workflow_format_is_parsed() {
    // The `<file>=<sha256>` lines `release.yml` writes.
    let sums = format!(
        "wd-linux-rust-ui-3.5.2=adbc10d4ac1cf5b816db60b777ffe6373ecb19e59f6fc7f8fb4f343e414f69bd\n\
         whisper-dictate-windows-3.5.2.zip=282ee9de08b5c3610953627ffe191d41a5ca20c65545e8a7aabbcdfefa8e5b09\n\
         {SETUP}={SETUP_HASH}\n"
    );
    assert_eq!(expected_sha256(&sums, SETUP), Ok(SETUP_HASH.to_owned()));
}

#[test]
fn sha256sums_coreutils_format_is_parsed() {
    let text = format!("{SETUP_HASH}  {SETUP}\r\n");
    assert_eq!(expected_sha256(&text, SETUP), Ok(SETUP_HASH.to_owned()));
    let binary = format!("{SETUP_HASH} *{SETUP}\n");
    assert_eq!(expected_sha256(&binary, SETUP), Ok(SETUP_HASH.to_owned()));
}

#[test]
fn sha256sums_hash_is_lowercased() {
    let sums = format!("{SETUP}={}", SETUP_HASH.to_ascii_uppercase());
    assert_eq!(expected_sha256(&sums, SETUP), Ok(SETUP_HASH.to_owned()));
}

#[test]
fn sha256sums_without_the_asset_is_refused() {
    let sums = format!("whisper-dictate-windows-setup-3.5.2.exe.sig={SETUP_HASH}\n# {SETUP}\n");
    assert_eq!(
        expected_sha256(&sums, SETUP),
        Err(ChecksumError::Missing(SETUP.to_owned()))
    );
    assert_eq!(
        expected_sha256("", SETUP),
        Err(ChecksumError::Missing(SETUP.to_owned()))
    );
}

#[test]
fn sha256sums_with_a_malformed_hash_is_refused() {
    let bad_digit = format!("{}zz", &SETUP_HASH[2..]);
    for hash in ["", "abc", &SETUP_HASH[1..], bad_digit.as_str()] {
        assert_eq!(
            expected_sha256(&format!("{SETUP}={hash}"), SETUP),
            Err(ChecksumError::Malformed(SETUP.to_owned())),
            "hash {hash:?}"
        );
    }
}

#[test]
fn sha256sums_with_conflicting_entries_is_refused() {
    let other = "0".repeat(64);
    let sums = format!("{SETUP}={SETUP_HASH}\n{SETUP}={other}\n");
    assert_eq!(
        expected_sha256(&sums, SETUP),
        Err(ChecksumError::Conflicting(SETUP.to_owned()))
    );
    // A repeated identical entry is fine.
    let repeated = format!("{SETUP}={SETUP_HASH}\n{SETUP_HASH}  {SETUP}\n");
    assert_eq!(expected_sha256(&repeated, SETUP), Ok(SETUP_HASH.to_owned()));
}

#[test]
fn checksum_errors_name_the_asset() {
    assert_eq!(
        ChecksumError::Missing(SETUP.to_owned()).to_string(),
        format!("sha256sums.txt has no entry for {SETUP}")
    );
}
