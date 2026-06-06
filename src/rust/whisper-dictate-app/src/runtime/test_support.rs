use super::*;
use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn runtime_module_args() -> Vec<String> {
    vec![
        "-m".to_owned(),
        "whisper_dictate.runtime".to_owned(),
        "--app-root".to_owned(),
        "/tmp/whisper-dictate".to_owned(),
    ]
}

pub(super) struct EnvVarGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvVarGuard {
    pub(super) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        env::set_var(key, value);
        Self { key, original }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.key, value);
        } else {
            env::remove_var(self.key);
        }
    }
}
