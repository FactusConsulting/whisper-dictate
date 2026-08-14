use super::whisper_model_setup::{setup_banner_for_entry, setup_banner_message};
use crate::ui::app::WHISPER_MODEL_PATH_ENV;
use crate::ui::whisper_models_state::ModelAvailability;

#[test]
fn setup_banner_hides_verified_model() {
    assert_eq!(
        setup_banner_message(
            false,
            true,
            true,
            Some(ModelAvailability::Available),
            "large-v3"
        ),
        None
    );
}

#[test]
fn setup_banner_prioritizes_invalid_external_path_over_cached_model() {
    let message = setup_banner_message(
        true,
        true,
        true,
        Some(ModelAvailability::Available),
        "large-v3",
    )
    .unwrap();
    assert!(message.contains(WHISPER_MODEL_PATH_ENV));
    assert!(message.contains("does not point to an existing"));
}

#[test]
fn setup_banner_hides_verified_retained_legacy_model() {
    assert_eq!(
        setup_banner_for_entry(false, Some("small"), false, "small", |_| {
            ModelAvailability::Available
        },),
        None
    );
}

#[test]
fn setup_banner_explains_invalid_external_path() {
    let message = setup_banner_message(true, true, true, None, "large-v3").unwrap();
    assert!(message.contains(WHISPER_MODEL_PATH_ENV));
    assert!(message.contains("does not point to an existing"));
}

#[test]
fn setup_banner_explains_unsupported_model() {
    let message = setup_banner_message(false, false, false, None, "unknown").unwrap();
    assert!(message.contains("unknown is not supported"));
}

#[test]
fn setup_banner_explains_retained_legacy_model() {
    let message = setup_banner_message(false, true, false, None, "small").unwrap();
    assert!(message.contains("small is a retained legacy model"));
    assert!(message.contains("wd models download small"));
}

#[test]
fn setup_banner_blocks_while_verifying() {
    let message = setup_banner_message(
        false,
        true,
        true,
        Some(ModelAvailability::Checking),
        "large-v3",
    )
    .unwrap();
    assert!(message.contains("Verifying large-v3"));
    assert!(message.contains("Recording stays disabled"));
}

#[test]
fn setup_banner_requests_missing_model_download() {
    let message = setup_banner_message(
        false,
        true,
        true,
        Some(ModelAvailability::Missing),
        "large-v3-turbo",
    )
    .unwrap();
    assert_eq!(
        message,
        "Download large-v3-turbo before starting local dictation."
    );
}
