use std::env;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandHookResult {
    pub enabled: bool,
    pub command: String,
    pub returncode: Option<i32>,
    pub latency_ms: u128,
    pub timeout: bool,
    pub error: Option<String>,
}

impl Default for CommandHookResult {
    fn default() -> Self {
        Self {
            enabled: false,
            command: String::new(),
            returncode: None,
            latency_ms: 0,
            timeout: false,
            error: None,
        }
    }
}

pub fn handle_command_hook() -> Result<()> {
    let event = read_stdin_json()?;
    let result = run_command_hook(&event);
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn run_command_hook(event: &Value) -> CommandHookResult {
    let command = command_setting();
    if command.trim().is_empty() {
        return CommandHookResult::default();
    }
    let timeout_ms = timeout_ms_setting();
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let started = Instant::now();

    let argv = match parse_command(&command) {
        Ok(argv) if !argv.is_empty() => argv,
        Ok(_) => return CommandHookResult::default(),
        Err(err) => {
            return CommandHookResult {
                enabled: true,
                command,
                latency_ms: started.elapsed().as_millis(),
                error: Some(err.to_string()),
                ..CommandHookResult::default()
            }
        }
    };

    match run_argv(&argv, event, timeout) {
        Ok((returncode, stderr, timed_out)) => CommandHookResult {
            enabled: true,
            command,
            returncode,
            latency_ms: started.elapsed().as_millis(),
            timeout: timed_out,
            error: if timed_out {
                Some(format!("command hook timed out after {timeout_ms}ms"))
            } else {
                trim_error(stderr)
            },
        },
        Err(err) => CommandHookResult {
            enabled: true,
            command,
            latency_ms: started.elapsed().as_millis(),
            error: Some(err.to_string()),
            ..CommandHookResult::default()
        },
    }
}

pub fn parse_command(command: &str) -> Result<Vec<String>> {
    let command = command.trim();
    if command.is_empty() {
        return Ok(Vec::new());
    }
    if command.starts_with('[') {
        let parsed: Value = serde_json::from_str(command)?;
        let Some(items) = parsed.as_array() else {
            return Err(anyhow!(
                "VOICEPI_COMMAND_HOOK JSON form must be an array of strings"
            ));
        };
        let mut argv = Vec::with_capacity(items.len());
        for item in items {
            let Some(text) = item.as_str() else {
                return Err(anyhow!(
                    "VOICEPI_COMMAND_HOOK JSON form must be an array of strings"
                ));
            };
            argv.push(text.to_owned());
        }
        return Ok(argv);
    }
    split_command_line(command)
}

fn run_argv(
    argv: &[String],
    event: &Value,
    timeout: Duration,
) -> Result<(Option<i32>, String, bool)> {
    let started = Instant::now();
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(serde_json::to_string(event)?.as_bytes())?;
    }

    loop {
        if let Some(status) = child.try_wait()? {
            let stderr = read_child_stderr(&mut child);
            return Ok((status.code(), stderr, false));
        }
        if started.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    child.kill().ok();
    child.wait().ok();
    let stderr = read_child_stderr(&mut child);
    Ok((None, stderr, true))
}

fn read_child_stderr(child: &mut std::process::Child) -> String {
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr).ok();
    }
    stderr
}

fn trim_error(stderr: String) -> Option<String> {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        None
    } else {
        let tail = trimmed
            .chars()
            .rev()
            .take(1000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some(tail)
    }
}

fn command_setting() -> String {
    env::var("VOICEPI_COMMAND_HOOK")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            config::load_settings()
                .map(|settings| settings.command_hook)
                .unwrap_or_default()
        })
}

fn timeout_ms_setting() -> u64 {
    env::var("VOICEPI_COMMAND_HOOK_TIMEOUT_MS")
        .ok()
        .or_else(|| {
            config::load_settings()
                .ok()
                .map(|settings| settings.command_hook_timeout_ms)
        })
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .map(|value| value.max(1.0) as u64)
        .unwrap_or(2000)
}

fn split_command_line(command: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    current.push(ch);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            (_, c) => current.push(c),
        }
    }
    if let Some(q) = quote {
        return Err(anyhow!("unterminated quote {q} in VOICEPI_COMMAND_HOOK"));
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

fn read_stdin_json() -> Result<Value> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_array_command() {
        assert_eq!(
            parse_command(r#"["python","-c","print(1)"]"#).unwrap(),
            vec!["python", "-c", "print(1)"]
        );
    }

    #[test]
    fn rejects_non_string_json_command_items() {
        let err = parse_command(r#"["ok",5]"#).unwrap_err().to_string();
        assert!(err.contains("array of strings"));
    }

    #[test]
    fn parses_quoted_command_string() {
        assert_eq!(
            parse_command(r#"tool --name "Claude Code" 'two words'"#).unwrap(),
            vec!["tool", "--name", "Claude Code", "two words"]
        );
    }

    #[test]
    fn rejects_unterminated_quoted_command_string() {
        let err = parse_command(r#"tool "unfinished"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unterminated quote"));
    }

    #[test]
    fn trim_error_keeps_utf8_tail_without_splitting_codepoints() {
        let raw = format!("{}\nrødgrød", "x".repeat(1200));
        let trimmed = trim_error(raw).unwrap();

        assert!(trimmed.ends_with("rødgrød"));
        assert!(trimmed.chars().count() <= 1000);
    }

    #[test]
    fn run_argv_reports_nonzero_exit_and_stderr() {
        let argv = failing_command();

        let (returncode, stderr, timed_out) = run_argv(
            &argv,
            &serde_json::json!({"text": "hello"}),
            Duration::from_secs(5),
        )
        .unwrap();

        assert!(!timed_out);
        assert_ne!(returncode, Some(0));
        assert!(stderr.contains("hook-error"));
    }

    #[test]
    fn run_argv_times_out_and_kills_child() {
        let argv = slow_command();

        let (returncode, _stderr, timed_out) = run_argv(
            &argv,
            &serde_json::json!({"text": "hello"}),
            Duration::from_millis(20),
        )
        .unwrap();

        assert!(timed_out);
        assert_eq!(returncode, None);
    }

    #[cfg(windows)]
    fn failing_command() -> Vec<String> {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            "echo hook-error 1>&2 & exit /b 7".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn failing_command() -> Vec<String> {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo hook-error >&2; exit 7".to_owned(),
        ]
    }

    #[cfg(windows)]
    fn slow_command() -> Vec<String> {
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            "ping -n 3 127.0.0.1 >NUL".to_owned(),
        ]
    }

    #[cfg(not(windows))]
    fn slow_command() -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), "sleep 2".to_owned()]
    }
}
