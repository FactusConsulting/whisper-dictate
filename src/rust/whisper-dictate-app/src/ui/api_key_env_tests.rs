use super::test_support::{EnvVarGuard, ENV_TEST_LOCK};
use super::*;
use std::env;

#[test]
fn provider_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-test-key");

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("groq-test-key")
    );
    assert_eq!(load_stt_api_key_from_env(CloudProvider::OpenAi), None);

    unsafe {
        env::set_var("VOICEPI_STT_API_KEY", "shared-test-key");
    }

    assert_eq!(
        load_stt_api_key_from_env(CloudProvider::Groq).as_deref(),
        Some("shared-test-key")
    );
}

#[test]
fn post_api_key_can_load_from_environment_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let _post = EnvVarGuard::remove("VOICEPI_POST_API_KEY");
    let _stt = EnvVarGuard::remove("VOICEPI_STT_API_KEY");
    let _openai = EnvVarGuard::remove("OPENAI_API_KEY");
    let _groq = EnvVarGuard::set("GROQ_API_KEY", "groq-post-key");

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("groq-post-key")
    );
    assert_eq!(load_post_api_key_from_env(PostProvider::OpenAi), None);

    unsafe {
        env::set_var("VOICEPI_POST_API_KEY", "post-override-key");
    }

    assert_eq!(
        load_post_api_key_from_env(PostProvider::Groq).as_deref(),
        Some("post-override-key")
    );
}
