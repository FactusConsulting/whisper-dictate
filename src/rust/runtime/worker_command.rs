//! Native runtime launch configuration.
//!
//! [`WorkerCommand`] is retained as the supervisor/UI configuration envelope:
//! its environment vector carries the resolved settings and credentials that
//! the in-process runtime materialises before installing native backends. It
//! no longer describes or launches a Python worker.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use crate::config;

pub(crate) const APP_ROOT_ENV: &str = "VOICEPI_APP_ROOT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: Vec<(String, String)>,
}

impl WorkerCommand {
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// Build the native runtime configuration envelope.
///
/// `program` is the CLI binary only for truthful diagnostics; the supervisor
/// consumes this value in-process and never spawns it.
pub fn worker_command(app_root: impl AsRef<Path>) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    build_worker_command(app_root, config::worker_env_overrides())
}

pub(crate) fn worker_command_with_ambient_env(
    app_root: impl AsRef<Path>,
    ambient_env: &BTreeMap<String, String>,
) -> WorkerCommand {
    let app_root = app_root.as_ref().to_path_buf();
    build_worker_command(app_root, config::worker_env_overrides_from_env(ambient_env))
}

fn build_worker_command(app_root: PathBuf, env: Vec<(String, String)>) -> WorkerCommand {
    if crate::diag::debug_enabled() {
        crate::diag::log!(
            "[runtime/debug] native configuration materialized app_root={} env_entries={}",
            app_root.display(),
            env.len()
        );
    }
    if crate::diag::trace_enabled() {
        let mut names: Vec<&str> = env.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        crate::diag::log!(
            "[runtime/trace] native configuration env_names={:?} values=redacted",
            names
        );
    }
    WorkerCommand {
        program: cli_exe_path(),
        args: Vec::new(),
        working_dir: app_root,
        env,
    }
}

pub fn default_worker_command() -> WorkerCommand {
    worker_command(app_root())
}

pub(crate) fn default_worker_command_with_ambient_env(
    ambient_env: &BTreeMap<String, String>,
) -> WorkerCommand {
    worker_command_with_ambient_env(app_root(), ambient_env)
}

/// Root used to resolve bundled native resources such as
/// `benchmark/corpus.json`.
pub fn resource_app_root() -> PathBuf {
    app_root()
}

/// Path to the console-subsystem CLI binary.
///
/// The GUI executable is a sibling Windows-subsystem binary with no CLI
/// surface, so resolve GUI executable names to the adjacent `wd[.exe]`.
pub fn cli_exe_path() -> PathBuf {
    let current = env::current_exe().unwrap_or_else(|_| PathBuf::from("wd"));
    cli_exe_from(&current)
}

pub(crate) fn cli_exe_from(current: &Path) -> PathBuf {
    let Some(file_name) = current.file_name().and_then(|f| f.to_str()) else {
        return current.to_path_buf();
    };
    let lower = file_name.to_ascii_lowercase();
    let (stem, has_exe) = match lower.strip_suffix(".exe") {
        Some(rest) => (rest, true),
        None => (lower.as_str(), false),
    };
    match stem {
        "wd-gui" | "whisper-dictate-gui" => {
            current.with_file_name(if has_exe { "wd.exe" } else { "wd" })
        }
        _ => current.to_path_buf(),
    }
}

/// Source-checkout fallback for native resources.
pub(crate) fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

pub(crate) fn app_root() -> PathBuf {
    if let Some(raw) = env::var_os(APP_ROOT_ENV) {
        return PathBuf::from(raw);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(root) = app_root_from_exe_path(&exe) {
            return root;
        }
    }
    source_root()
}

pub(crate) fn app_root_from_exe_path(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?;
    root.join("benchmark")
        .join("corpus.json")
        .exists()
        .then(|| root.to_path_buf())
}
