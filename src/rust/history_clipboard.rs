use anyhow::{anyhow, Result};
use std::process::{Command, Stdio};

pub(super) fn run(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    run_command(command, text)
}

fn run_command(mut command: Command, text: &str) -> Result<()> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
    crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
    let output = crate::bounded_process::run_clipboard_owner(
        &mut command,
        text.as_bytes().to_vec(),
        crate::bounded_process::HELPER_TIMEOUT,
    )?;
    if output.completion != crate::bounded_process::Completion::Exited {
        return Err(anyhow!("clipboard helper timed out after 10s"));
    }
    if let Some(error) = output.stdin_error {
        return Err(error.into());
    }
    if !output.status.success() {
        return Err(anyhow!("exit status {}", output.status));
    }
    Ok(())
}

#[cfg(test)]
#[path = "history_clipboard_tests.rs"]
mod tests;
