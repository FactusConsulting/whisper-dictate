use super::test_support::{EnvVarGuard, ENV_LOCK};
use super::*;

#[test]
fn provider_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-test-key");

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("groq-test-key")
    );
    assert_eq!(load_stt_api_key_from_env(CloudProvider::OpenAi), None);

    // Promote the earlier `remove` guard into a fresh `set` guard so the
    // mutation is RAII: on Drop the original value (None, captured by the
    // outer `_stt` `remove` guard) is restored even if an assert below
    // panics. A bare `env::set_var` here previously leaked
    // VOICEPI_STT_API_KEY=shared-test-key into every later test on panic
    // (Codex P2 #415 pattern).
    drop(_stt);
    let _stt = EnvVarGuard::set("VOICEPI_STT_API_KEY", "shared-test-key");

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("shared-test-key")
    );
}

#[test]
fn post_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _post = EnvVarGuard::remove("VOICEPI_POST_API_KEY");
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-post-key");

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("groq-post-key")
    );
    assert_eq!(load_post_api_key_from_env(PostProvider::OpenAi), None);

    // See note in the STT variant: the second-stage mutation must be
    // RAII-guarded so a panic between this line and the end of the test
    // does not leak VOICEPI_POST_API_KEY=post-override-key into every
    // sibling test running in the same library binary.
    drop(_post);
    let _post = EnvVarGuard::set("VOICEPI_POST_API_KEY", "post-override-key");

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("post-override-key")
    );
}
