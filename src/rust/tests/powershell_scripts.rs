//! Behavioral coverage for the Windows PowerShell helpers used by CI and releases.

#[cfg(windows)]
mod common;

#[cfg(windows)]
mod windows {
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output};

    use serde_json::Value;

    use super::common::repo_root;

    fn run_probe(fake_cargo: &str, args: &[&str]) -> Output {
        let temp = tempfile::tempdir().expect("probe temp directory");
        fs::write(temp.path().join("cargo.cmd"), fake_cargo).expect("fake cargo");
        let mut path_entries = vec![temp.path().to_path_buf()];
        if let Some(existing) = env::var_os("PATH") {
            path_entries.extend(env::split_paths(&existing));
        }
        let path = env::join_paths(path_entries).expect("probe PATH");
        let mut command = Command::new("pwsh");
        command
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "scripts/dev/probe-key.ps1",
            ])
            .args(args)
            .env("PATH", path)
            .current_dir(repo_root());
        command.output().expect("run probe-key.ps1")
    }

    #[test]
    fn probe_key_keeps_json_stdout_separate_from_diagnostics() {
        let output = run_probe(
            "@echo off\necho {\"events\":0}\necho diagnostic-from-cargo 1>&2\nexit /b 0\n",
            &["-Json"],
        );
        assert!(output.status.success(), "probe failed: {output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(serde_json::from_str::<Value>(stdout.trim()).is_ok());
        assert!(!stdout.contains("diagnostic-from-cargo"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic-from-cargo"));
    }

    #[test]
    fn probe_key_preserves_all_verdict_exit_codes() {
        let matched = run_probe(
            "@echo off\necho Events: 1\nexit /b 0\n",
            &["-Chord", "ctrl"],
        );
        assert_eq!(matched.status.code(), Some(2), "{matched:?}");

        let unknown = run_probe(
            "@echo off\necho unknown key: ctrl 1>&2\nexit /b 1\n",
            &["-Chord", "ctrl"],
        );
        assert_eq!(unknown.status.code(), Some(3), "{unknown:?}");

        let failed = run_probe(
            "@echo off\necho native failure 1>&2\nexit /b 1\n",
            &["-Chord", "ctrl"],
        );
        assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    }

    fn write_crlf(path: &Path, contents: &str) {
        fs::write(path, contents.replace('\n', "\r\n")).expect("write fixture");
    }

    fn write_version_fixture(root: &Path, version: &str) {
        fs::create_dir_all(root.join("src/rust")).expect("cargo directory");
        fs::create_dir_all(root.join("nix")).expect("nix directory");
        write_crlf(&root.join("VERSION"), &format!("{version}\n"));
        write_crlf(
            &root.join("src/rust/Cargo.toml"),
            &format!("[package]\nname = \"whisper-dictate-app\"\nversion = \"{version}\"\n"),
        );
        write_crlf(
            &root.join("src/rust/Cargo.lock"),
            &format!("[[package]]\nname = \"whisper-dictate-app\"\nversion = \"{version}\"\n"),
        );
        write_crlf(
            &root.join("nix/package.nix"),
            &format!("version ? \"{version}\"\n"),
        );
    }

    fn run_bump_version(root: &Path, args: &[&str]) -> Output {
        Command::new("pwsh")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "scripts/dev/bump-version.ps1",
            ])
            .args(args)
            .arg("-Root")
            .arg(root)
            .current_dir(repo_root())
            .output()
            .expect("run bump-version.ps1")
    }

    fn output_text(output: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    #[test]
    fn dev_check_uses_rancher_desktops_windows_default_context() {
        let script = fs::read_to_string(repo_root().join("scripts/dev/dev-check.ps1"))
            .expect("read dev check script");

        assert!(script.contains("$DockerContext = 'default'"));
        assert!(script.contains("Rancher Desktop"));
        assert!(!script.contains("$DockerContext = 'desktop-linux'"));
        assert!(script.contains("GIT_CONFIG_KEY_0=safe.directory"));
        assert!(script.contains("GIT_CONFIG_VALUE_0=/repo"));
    }

    #[test]
    fn bump_version_updates_all_files_and_preserves_lockfile_line_endings() {
        let temp = tempfile::tempdir().expect("version temp directory");
        write_version_fixture(temp.path(), "1.22.6");

        let output = run_bump_version(temp.path(), &["-Version", "1.23.0"]);
        assert!(output.status.success(), "bump failed: {output:?}");

        for relative in [
            "VERSION",
            "src/rust/Cargo.toml",
            "src/rust/Cargo.lock",
            "nix/package.nix",
        ] {
            let contents =
                fs::read_to_string(temp.path().join(relative)).expect("read bumped file");
            assert!(
                contents.contains("1.23.0"),
                "{relative} was not bumped: {contents}"
            );
        }
        let lock = fs::read(temp.path().join("src/rust/Cargo.lock")).expect("read lock bytes");
        assert_eq!(
            lock, b"[[package]]\r\nname = \"whisper-dictate-app\"\r\nversion = \"1.23.0\"\r\n",
            "Cargo.lock line endings or replacement changed"
        );
    }

    #[test]
    fn bump_version_check_accepts_matching_stable_and_rc_versions() {
        for version in ["1.22.6", "1.22.6-rc.1"] {
            let temp = tempfile::tempdir().expect("version temp directory");
            write_version_fixture(temp.path(), version);

            let output = run_bump_version(temp.path(), &["-Check", "-ExpectedVersion", version]);
            assert!(
                output.status.success(),
                "matching {version} failed: {}",
                output_text(&output)
            );
        }
    }

    #[test]
    fn bump_version_check_rejects_mismatched_and_malformed_release_versions() {
        let temp = tempfile::tempdir().expect("version temp directory");
        write_version_fixture(temp.path(), "1.22.6");

        let mismatch = run_bump_version(temp.path(), &["-Check", "-ExpectedVersion", "1.22.7"]);
        let mismatch_text = output_text(&mismatch);
        assert!(!mismatch.status.success(), "mismatch unexpectedly passed");
        assert!(mismatch_text.contains("RELEASE VERSION MISMATCH"));
        assert!(mismatch_text.contains("1.22.6"));
        assert!(mismatch_text.contains("1.22.7"));

        let malformed =
            run_bump_version(temp.path(), &["-Check", "-ExpectedVersion", "1.22.6-rc.0"]);
        let malformed_text = output_text(&malformed);
        assert!(!malformed.status.success(), "malformed version passed");
        assert!(malformed_text.contains("INVALID expected release version"));
        assert!(malformed_text.contains("1.22.6-rc.0"));
    }

    #[test]
    fn bump_version_check_rejects_disagreement_between_source_files() {
        let temp = tempfile::tempdir().expect("version temp directory");
        write_version_fixture(temp.path(), "1.22.6");
        write_crlf(
            &temp.path().join("nix/package.nix"),
            "version ? \"1.22.5\"\n",
        );

        let output = run_bump_version(temp.path(), &["-Check", "-ExpectedVersion", "1.22.6"]);
        assert!(!output.status.success(), "inconsistent files passed");
        assert!(output_text(&output).contains("INCONSISTENT version files"));
    }
}
