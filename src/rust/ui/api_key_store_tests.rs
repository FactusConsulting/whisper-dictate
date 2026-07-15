use super::test_support::{EnvVarGuard, ENV_TEST_LOCK};
use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn file_api_key_store_round_trips_and_deletes_stt_keys() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_secret(CloudProvider::Groq.credential_user(), " groq-secret ").unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::Groq.credential_user()).unwrap(),
        "groq-secret"
    );
    let raw = std::fs::read_to_string(&store).unwrap();
    assert!(raw.contains("stt-api-key:groq"));
    assert!(raw.contains("groq-secret"));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o600
    );

    save_file_secret(CloudProvider::Groq.credential_user(), "").unwrap();

    assert!(!store.exists());
}

#[test]
fn file_api_key_store_keeps_post_and_stt_keys_separate() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_secret(CloudProvider::OpenAi.credential_user(), "stt-secret").unwrap();
    save_file_secret(PostProvider::OpenAi.credential_user(), "post-secret").unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::OpenAi.credential_user()).unwrap(),
        "stt-secret"
    );
    assert_eq!(
        load_file_secret(PostProvider::OpenAi.credential_user()).unwrap(),
        "post-secret"
    );
}

#[test]
fn disabled_os_keyring_uses_file_store_through_ui_secret_api() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);
    let _disable_guard = EnvVarGuard::set(DISABLE_OS_KEYRING_ENV, "1");

    let report = save_stt_api_key(CloudProvider::Groq, " groq-secret ").unwrap();
    assert_eq!(report.location, SecretSaveLocation::File);
    assert_eq!(report.fallback_path, store);
    assert!(report.fallback_reason.contains(DISABLE_OS_KEYRING_ENV));
    assert!(report.status_label().contains("fallback key file"));
    assert_eq!(
        load_stt_api_key_state(CloudProvider::Groq).unwrap().0,
        "groq-secret"
    );

    save_stt_api_key(CloudProvider::Groq, "").unwrap();

    assert!(!store.exists());
}

#[test]
fn successful_keyring_save_keeps_file_fallback() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("api-keys.json");
    let store_env = store.to_string_lossy().to_string();
    let _store_guard = EnvVarGuard::set(SECRET_STORE_ENV, &store_env);

    save_file_fallback_after_keyring_success(
        CloudProvider::Groq.credential_user(),
        " groq-secret ",
    )
    .unwrap();

    assert_eq!(
        load_file_secret(CloudProvider::Groq.credential_user()).unwrap(),
        "groq-secret"
    );
}

#[test]
fn credential_store_report_status_does_not_claim_fallback_file() {
    let report = SecretSaveReport {
        location: SecretSaveLocation::CredentialStore,
        credential_service: "whisper-dictate",
        credential_user: "stt-api-key:groq".to_owned(),
        credential_target: "stt-api-key:groq.whisper-dictate".to_owned(),
        fallback_path: std::path::PathBuf::from("api-keys.json"),
        fallback_reason: "Windows Credential Manager read-back verified".to_owned(),
    };

    let status = report.status_label();
    let log_details = report.log_details();

    assert!(status.contains("Credential") || status.contains("credential"));
    assert!(!status.contains("fallback file"));
    assert!(log_details.contains("stored=credential_store"));
    assert!(log_details.contains("credential_target=stt-api-key:groq.whisper-dictate"));
    assert!(!log_details.contains("fallback_file="));
}

#[test]
fn fallback_report_names_credential_failure_and_fallback_path() {
    let report = SecretSaveReport {
        location: SecretSaveLocation::File,
        credential_service: "whisper-dictate",
        credential_user: "stt-api-key:groq".to_owned(),
        credential_target: "stt-api-key:groq.whisper-dictate".to_owned(),
        fallback_path: std::path::PathBuf::from("api-keys.json"),
        fallback_reason: "OS credential store failed: unavailable".to_owned(),
    };

    let status = report.status_label();
    let log_details = report.log_details();

    assert!(status.contains("fallback key file"));
    assert!(status.contains("OS credential store failed"));
    assert!(log_details.contains("stored=fallback_file"));
    assert!(log_details.contains("fallback_file=api-keys.json"));
    assert!(log_details.contains("reason=OS credential store failed"));
}
