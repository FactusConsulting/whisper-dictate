// Re-export the crate-wide env lock + guard under the historical names.
// Per-module locks/guards cannot serialise against tests in OTHER modules
// that mutate the same process env, so a single crate-wide design is the
// only sound one. See `crate::test_env_lock` for the full story.
pub(super) use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

pub(super) fn runtime_module_args() -> Vec<String> {
    vec![
        "-m".to_owned(),
        "whisper_dictate.runtime".to_owned(),
        "--app-root".to_owned(),
        "/tmp/whisper-dictate".to_owned(),
    ]
}
