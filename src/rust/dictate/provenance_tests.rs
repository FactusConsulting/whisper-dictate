//! Unit tests for [`super`] -- the engine / STT-implementation
//! provenance vocabulary.
//!
//! These labels are a cross-language wire contract: the same strings are
//! emitted by `vp_dictate.py` on the Python worker. Pinning them here
//! (and in `src/python/tests/test_dictate.py`) means a rename on one side
//! fails a test rather than silently producing two incompatible schemas.

use super::*;

#[test]
fn engine_labels_are_the_documented_wire_values() {
    assert_eq!(ENGINE_RUST_IN_PROCESS, "rust-in-process");
}

#[test]
fn stt_impl_labels_are_the_documented_wire_values() {
    assert_eq!(STT_IMPL_WHISPER_CPP, "whisper.cpp");
    assert_eq!(STT_IMPL_CLOUD_OPENAI, "cloud-openai");
    assert_eq!(STT_IMPL_CLOUD_GROQ, "cloud-groq");
    assert_eq!(STT_IMPL_CLOUD_CUSTOM, "cloud-custom");
}

#[test]
fn every_label_is_ascii_and_space_free() {
    // They land in JSONL rows and on a console line; the console-ASCII
    // guard applies, and a space would break `key=value` parsing of the
    // startup line.
    for label in [
        ENGINE_RUST_IN_PROCESS,
        STT_IMPL_WHISPER_CPP,
        STT_IMPL_CLOUD_OPENAI,
        STT_IMPL_CLOUD_GROQ,
        STT_IMPL_CLOUD_CUSTOM,
    ] {
        assert!(label.is_ascii(), "{label:?} must be ASCII");
        assert!(!label.contains(' '), "{label:?} must not contain spaces");
    }
}

#[test]
fn groq_base_url_resolves_to_the_groq_impl() {
    // `stt_backend` is `openai` for Groq too, so the base URL is the only
    // signal that separates the two providers.
    assert_eq!(
        cloud_stt_impl_for_base_url("https://api.groq.com/openai/v1"),
        STT_IMPL_CLOUD_GROQ
    );
    assert_eq!(
        cloud_stt_impl_for_base_url("HTTPS://API.GROQ.COM/openai/v1"),
        STT_IMPL_CLOUD_GROQ
    );
}

#[test]
fn groq_classification_is_by_host_not_substring() {
    // #687: a substring test mislabels both directions, and either
    // way the record names a service that did not handle the audio.
    for url in [
        // Contains `groq.com` but the host is somebody else's.
        "https://groq.com.attacker.example/v1",
        "https://example.test/proxy/groq.com",
        // Userinfo trick: the host is `custom.example`.
        "https://api.groq.com@custom.example/v1",
    ] {
        assert_eq!(
            cloud_stt_impl_for_base_url(url),
            STT_IMPL_CLOUD_CUSTOM,
            "{url} must not be labelled groq -- its host is not groq.com"
        );
    }
    // A trailing DNS root dot and a port are the same host to a resolver.
    for url in [
        "https://api.groq.com./openai/v1",
        "https://api.groq.com:443/openai/v1",
        "https://groq.com/openai/v1",
    ] {
        assert_eq!(
            cloud_stt_impl_for_base_url(url),
            STT_IMPL_CLOUD_GROQ,
            "{url} is genuinely groq"
        );
    }
}

#[test]
fn openai_and_unset_base_urls_resolve_to_the_openai_impl() {
    for url in [
        "https://api.openai.com/v1",
        "https://openai.com/v1",
        "https://API.OPENAI.COM./v1",
        // Unset -- `DEFAULT_STT_BASE_URL` IS OpenAI.
        "",
        "   ",
    ] {
        assert_eq!(
            cloud_stt_impl_for_base_url(url),
            STT_IMPL_CLOUD_OPENAI,
            "{url:?} should resolve to OpenAI"
        );
    }
}

/// OpenAI-compatible endpoint, so a self-hosted / Azure / proxied URL
/// would otherwise be recorded as though OpenAI had served the audio.
#[test]
fn other_openai_compatible_endpoints_resolve_to_the_custom_impl() {
    for url in [
        // `vp_setup.py` exposes `custom` as a first-class provider.
        "http://127.0.0.1:8080/v1",
        "http://localhost:9000/v1",
        "https://my-resource.openai.azure.com/openai/v1",
        "https://stt.internal.example/v1",
        // Unparseable: we cannot claim it was OpenAI.
        "not a url at all",
    ] {
        assert_eq!(
            cloud_stt_impl_for_base_url(url),
            STT_IMPL_CLOUD_CUSTOM,
            "{url:?} is not OpenAI and must not be labelled as it"
        );
    }
}

#[test]
fn startup_line_matches_the_documented_shape() {
    assert_eq!(
        startup_line(
            ENGINE_RUST_IN_PROCESS,
            STT_IMPL_WHISPER_CPP,
            "vulkan",
            "large-v3-turbo"
        ),
        "[runtime] transcribe backend resolved: engine=rust-in-process \
         impl=whisper.cpp accel=vulkan model=large-v3-turbo"
    );
}

#[test]
fn startup_line_omits_an_empty_model_rather_than_emitting_a_blank_value() {
    let line = startup_line(
        ENGINE_RUST_IN_PROCESS,
        STT_IMPL_CLOUD_GROQ,
        "unknown",
        "   ",
    );
    assert_eq!(
        line,
        "[runtime] transcribe backend resolved: engine=rust-in-process \
         impl=cloud-groq accel=unknown"
    );
    assert!(
        !line.contains("model="),
        "blank model must be dropped: {line}"
    );
}

#[test]
fn startup_line_is_ascii() {
    let line = startup_line(
        ENGINE_RUST_IN_PROCESS,
        STT_IMPL_WHISPER_CPP,
        "cpu",
        "large-v3-turbo",
    );
    assert!(line.is_ascii(), "startup line must be ASCII: {line}");
}
