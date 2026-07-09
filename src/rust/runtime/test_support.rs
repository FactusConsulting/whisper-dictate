// Re-export the crate-wide env lock + guard under the historical names.
// Per-module locks/guards cannot serialise against tests in OTHER modules
// that mutate the same process env, so a single crate-wide design is the
// only sound one. See `crate::test_env_lock` for the full story.
#[allow(unused_imports)]
// Wave 8 Part 2: some test files were deleted; imports may not have callers on stock feature builds.
pub(super) use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};
