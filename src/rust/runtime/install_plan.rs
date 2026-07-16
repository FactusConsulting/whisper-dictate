//! Install-time planning for the Python runtime dependencies.
//!
//! Split out of `runtime.rs` in the 500-LOC modularity refactor. Owns
//! the [`InstallPlan`] resolver + runner plus the small helpers
//! ([`pip_install_command`], [`run_install_command`],
//! [`wants_cuda_runtime`], [`requirements_path`],
//! [`first_existing_requirements`]) it composes with.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

use crate::config;

use super::process::configure_background_process;
use super::worker_command::{
    app_root, default_python_name, default_venv_dir, home_dir, venv_python_path, PlannedCommand,
    Platform, PYTHON_ENV,
};

pub(crate) const BOOTSTRAP_PYTHON_ENV: &str = "VOICEPI_BOOTSTRAP_PYTHON";

pub fn install() -> Result<()> {
    let plan = InstallPlan::for_current_environment(app_root())?;
    plan.run()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallPlan {
    pub(super) app_root: PathBuf,
    pub(super) requirements: PathBuf,
    pub(super) venv_python: PathBuf,
    pub(super) create_venv: Option<PlannedCommand>,
    pub(super) install_commands: Vec<PlannedCommand>,
}

impl InstallPlan {
    pub(crate) fn for_current_environment(app_root: PathBuf) -> Result<Self> {
        let requirements = requirements_path(&app_root)?;
        let platform = Platform::current();
        let bootstrap_python = env::var_os(BOOTSTRAP_PYTHON_ENV)
            .or_else(|| env::var_os(PYTHON_ENV))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default_python_name()));

        if let Some(override_python) = env::var_os(PYTHON_ENV) {
            let mut plan =
                Self::from_parts(app_root, requirements, PathBuf::from(override_python), None);
            plan.add_optional_requirements();
            return Ok(plan);
        }

        let home = home_dir().ok_or_else(|| anyhow!("HOME/USERPROFILE is not set"))?;
        let venv_dir = default_venv_dir(&home, platform);
        let venv_python = venv_python_path(&venv_dir, platform);
        let create_venv = (!venv_python.exists()).then(|| PlannedCommand {
            program: bootstrap_python,
            args: vec![
                "-m".to_owned(),
                "venv".to_owned(),
                venv_dir.display().to_string(),
            ],
            working_dir: app_root.clone(),
        });

        let mut plan = Self::from_parts(app_root, requirements, venv_python, create_venv);
        plan.add_optional_requirements();
        Ok(plan)
    }

    pub(crate) fn from_parts(
        app_root: PathBuf,
        requirements: PathBuf,
        venv_python: PathBuf,
        create_venv: Option<PlannedCommand>,
    ) -> Self {
        let install_commands = vec![
            PlannedCommand {
                program: venv_python.clone(),
                args: vec![
                    "-m".to_owned(),
                    "pip".to_owned(),
                    "install".to_owned(),
                    "--upgrade".to_owned(),
                    "pip".to_owned(),
                ],
                working_dir: app_root.clone(),
            },
            pip_install_command(&venv_python, &requirements, &app_root),
        ];

        Self {
            app_root,
            requirements,
            venv_python,
            create_venv,
            install_commands,
        }
    }

    fn add_optional_requirements(&mut self) {
        if wants_cuda_runtime() {
            if let Some(requirements) = first_existing_requirements(
                &self.app_root,
                &["requirements/gpu.txt", "requirements-gpu.txt"],
            ) {
                self.install_commands.push(pip_install_command(
                    &self.venv_python,
                    &requirements,
                    &self.app_root,
                ));
            }
        }
        // Wave 8 of #348 removed the optional Parakeet/NeMo install
        // step here together with the backend itself; the only optional
        // requirements file the installer still appends is the CUDA
        // bundle gated above on `wants_cuda_runtime()`.
    }

    pub(crate) fn run(&self) -> Result<()> {
        println!(
            "Installing whisper-dictate runtime with {}",
            self.venv_python.display()
        );
        println!("Requirements: {}", self.requirements.display());
        if let Some(command) = &self.create_venv {
            run_install_command(command)?;
        }
        for command in &self.install_commands {
            run_install_command(command)?;
        }
        println!("Install complete. Run `whisper-dictate doctor` to verify the runtime.");
        Ok(())
    }
}

fn pip_install_command(venv_python: &Path, requirements: &Path, app_root: &Path) -> PlannedCommand {
    PlannedCommand {
        program: venv_python.to_path_buf(),
        args: vec![
            "-m".to_owned(),
            "pip".to_owned(),
            "install".to_owned(),
            "-r".to_owned(),
            requirements.display().to_string(),
        ],
        working_dir: app_root.to_path_buf(),
    }
}

fn run_install_command(command: &PlannedCommand) -> Result<()> {
    println!("> {}", command.display());
    let mut process = Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.working_dir);
    configure_background_process(&mut process);
    let status = process.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("install command failed with status {status}"))
    }
}

fn wants_cuda_runtime() -> bool {
    if env::var("VOICEPI_DEVICE").is_ok_and(|value| value.eq_ignore_ascii_case("cuda")) {
        return true;
    }
    config::load_settings()
        .map(|settings| settings.device.eq_ignore_ascii_case("cuda"))
        .unwrap_or(false)
}

pub(crate) fn requirements_path(app_root: &Path) -> Result<PathBuf> {
    if let Some(path) = first_existing_requirements(
        app_root,
        &[
            "requirements/cpu.txt",
            "requirements/gpu.txt",
            "requirements-cpu.txt",
            "requirements-gpu.txt",
            "requirements.txt",
        ],
    ) {
        return Ok(path);
    }
    Err(anyhow!(
        "no requirements file found in {}",
        app_root.display()
    ))
}

fn first_existing_requirements(app_root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|candidate| {
            candidate
                .split('/')
                .fold(app_root.to_path_buf(), |path, part| path.join(part))
        })
        .find(|path| path.exists())
}
