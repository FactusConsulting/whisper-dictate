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

    #[test]
    fn bump_version_updates_all_files_and_preserves_lockfile_line_endings() {
        let temp = tempfile::tempdir().expect("version temp directory");
        fs::create_dir_all(temp.path().join("src/rust")).expect("cargo directory");
        fs::create_dir_all(temp.path().join("nix")).expect("nix directory");
        write_crlf(&temp.path().join("VERSION"), "1.22.6\n");
        write_crlf(
            &temp.path().join("src/rust/Cargo.toml"),
            "[package]\nname = \"whisper-dictate-app\"\nversion = \"1.22.6\"\n",
        );
        write_crlf(
            &temp.path().join("src/rust/Cargo.lock"),
            "[[package]]\nname = \"whisper-dictate-app\"\nversion = \"1.22.6\"\n",
        );
        write_crlf(
            &temp.path().join("nix/package.nix"),
            "version ? \"1.22.6\"\n",
        );

        let output = Command::new("pwsh")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "scripts/dev/bump-version.ps1",
                "-Version",
                "1.23.0",
                "-Root",
            ])
            .arg(temp.path())
            .current_dir(repo_root())
            .output()
            .expect("run bump-version.ps1");
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
}
