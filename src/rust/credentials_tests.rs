//! Sibling regression tests for [`crate::credentials`].
//!
//! The bulk of the resolution-logic tests live in the module-body
//! `#[cfg(test)] mod tests` inside `credentials.rs` -- keeping the
//! injectable `resolve_with` next to the tests that pin its precedence.
//! This sibling file exists so the regression-test discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) sees a
//! matching test file for the module, and holds the tests that go
//! through the crate-public API only.

use super::credentials::Provider;

const GROQ_URL: &str = "https://api.groq.com/openai/v1";
const OPENAI_URL: &str = "https://api.openai.com/v1";
const CUSTOM_URL: &str = "https://llm.internal.example/v1";

#[test]
fn provider_classification_is_the_endpoint_credential_lookup_uses() {
    // The P1 finding on PR #615 named `Provider::from_base_url` as the
    // classifier that decides which stored account holds the key. Pin
    // the three cases exercised end-to-end so a change in host parsing
    // is caught by a scanner-visible test file, not only by the
    // in-module unit test that also lives in `credentials.rs`.
    assert_eq!(Provider::from_base_url(GROQ_URL), Provider::Groq);
    assert_eq!(Provider::from_base_url(OPENAI_URL), Provider::OpenAi);
    assert_eq!(Provider::from_base_url(CUSTOM_URL), Provider::Custom);
}

#[test]
fn provider_classification_ignores_url_lookalikes() {
    // Same shape as the module-body test but through the crate boundary
    // -- reachable from an integration harness even if `resolve_with`
    // is one day moved behind a doc(hidden) or feature-gated interface.
    // A userinfo prefix and a suffix trap both classify as Custom, so
    // no stored provider key can ever reach an unrelated host.
    assert_eq!(
        Provider::from_base_url("https://api.groq.com@evil.example/v1"),
        Provider::Custom
    );
    assert_eq!(
        Provider::from_base_url("https://groq.com.attacker.example/v1"),
        Provider::Custom
    );
}
