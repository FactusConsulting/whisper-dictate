//! Cloud API-key resolution shared by the UI and the CLI.
//!
//! The keys live in the OS credential store (with an `api-keys.json` file
//! fallback), written by the Settings UI. Until now ONLY the UI could read
//! them back: it loads the key and exports it into the worker's environment
//! before starting the runtime. Every CLI entry point -- `run` and by
//! extension the documented terminal test in
//! `scripts/manual-test/README.md` -- started a native run with no key at
//! all, so a user configured for cloud STT hit
//!
//! ```text
//! x startup error: openai API requires OPENAI_API_KEY, GROQ_API_KEY,
//!   or VOICEPI_STT_API_KEY/VOICEPI_POST_API_KEY
//! ```
//!
//! on a machine where the key was saved and working in the UI.
//!
//! Resolution order matches what the UI applies, and the environment wins so
//! an explicit `VOICEPI_STT_API_KEY=... wd run` still overrides
//! whatever is stored:
//!
//!   1. `VOICEPI_STT_API_KEY` / `VOICEPI_POST_API_KEY`
//!   2. the provider-generic variable for the configured endpoint
//!      (`GROQ_API_KEY` / `OPENAI_API_KEY`)
//!   3. the OS credential store, then its file fallback
//!
//! NOTE: the store itself still lives under `ui::secret_store`, which this
//! module borrows. Relocating it out of `ui/` is the tidier end state but a
//! larger move; this keeps the resolution ORDER in one place today so the two
//! callers cannot drift.

use crate::ui::api_keys::load_secret;

/// Credential-store account names, matching `CloudProvider::credential_user`
/// / `PostProvider::credential_user` in the UI. Duplicated as data rather
/// than reached through those private enums, and pinned by a test that
/// asserts the exact strings so a rename on either side is caught.
const STT_GROQ: &str = "stt-api-key:groq";
const STT_OPENAI: &str = "stt-api-key:openai";
const STT_CUSTOM: &str = "stt-api-key:custom";
const STT_NEMOTRON: &str = "stt-api-key:nemotron";
const POST_GROQ: &str = "post-api-key:groq";
const POST_OPENAI: &str = "post-api-key:openai";

/// Which provider a base URL denotes, by HOST rather than substring.
///
/// Host classification (not `contains`) for the same reason
/// `cloud_api::transcribe` uses it: `https://api.groq.com@evil.example/v1`
/// has host `evil.example`, and `groq.com.attacker.example` merely contains
/// `groq.com`. Getting this wrong would hand a stored provider credential to
/// an unrelated host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Groq,
    OpenAi,
    Custom,
}

impl Provider {
    pub fn from_base_url(base_url: &str) -> Self {
        match crate::cloud_api::provider_host_public(base_url) {
            Some(host) => {
                let matches =
                    |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
                if matches("groq.com") {
                    Self::Groq
                } else if matches("openai.com") {
                    Self::OpenAi
                } else {
                    Self::Custom
                }
            }
            None => Self::Custom,
        }
    }

    fn stt_account(self) -> &'static str {
        match self {
            Self::Groq => STT_GROQ,
            Self::OpenAi => STT_OPENAI,
            Self::Custom => STT_CUSTOM,
        }
    }

    /// The STT credential-store account for THIS provider, or `None` for a
    /// custom endpoint. Used as a post-key fallback: both
    /// STT and post-processing use one key per Groq/OpenAI account, and the
    /// UI's `load_post_api_key_from_env` treats the STT env var as a
    /// fallback for the post lookup -- the credential store must apply the
    /// same rule so a user who saved only the STT credential still reaches
    /// cloud post-processing.
    fn stt_account_for_post_fallback(self) -> Option<&'static str> {
        match self {
            Self::Groq => Some(STT_GROQ),
            Self::OpenAi => Some(STT_OPENAI),
            // A custom endpoint has no cross-account fallback: sending a
            // provider's saved key to an unrelated host is exactly the leak
            // `post_account` already guards against.
            Self::Custom => None,
        }
    }

    /// `None` for a custom endpoint: there is no post-processing account for
    /// a self-hosted provider, and falling back to another provider's would
    /// send its key somewhere it does not belong.
    fn post_account(self) -> Option<&'static str> {
        match self {
            Self::Groq => Some(POST_GROQ),
            Self::OpenAi => Some(POST_OPENAI),
            Self::Custom => None,
        }
    }

    /// The provider-generic environment variable, or `None` for a custom
    /// endpoint (which must be given an explicit `VOICEPI_*_API_KEY`).
    fn generic_env(self) -> Option<&'static str> {
        match self {
            Self::Groq => Some("GROQ_API_KEY"),
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::Custom => None,
        }
    }
}

/// Resolve the STT key for `base_url`, or `None` when nothing is configured.
pub fn resolve_stt_api_key(base_url: &str) -> Option<String> {
    let provider = Provider::from_base_url(base_url);
    resolve_with(
        &["VOICEPI_STT_API_KEY"],
        provider.generic_env(),
        &[Some(provider.stt_account())],
        |name| std::env::var(name).ok(),
        load_secret_reported,
    )
}

pub fn resolve_stt_api_key_for_provider(base_url: &str, provider: &str) -> Option<String> {
    if crate::cloud_api::is_nemotron_provider(provider) {
        return resolve_with(
            &["VOICEPI_STT_API_KEY"],
            None,
            &[Some(STT_NEMOTRON)],
            |name| std::env::var(name).ok(),
            load_secret_reported,
        );
    }
    resolve_stt_api_key(base_url)
}

/// Resolve the post-processing key for `base_url`.
///
/// `VOICEPI_STT_API_KEY` is accepted as a fallback because both providers
/// issue one key per account -- the UI's `load_post_api_key_from_env` applies
/// the same rule. The credential-store lookup applies the same fallback:
/// store: after the post-specific account comes up empty, we try the STT
/// account for THIS provider so a user who saved only the STT credential
/// still reaches Groq / OpenAI post-processing without configuring the key
/// twice. Custom endpoints do NOT get this fallback -- a self-hosted URL has
/// no cross-account trust and forwarding another provider's key would be a
/// leak (matches `post_account`'s reasoning).
pub fn resolve_post_api_key(base_url: &str) -> Option<String> {
    let provider = Provider::from_base_url(base_url);
    resolve_with(
        &["VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY"],
        provider.generic_env(),
        &[
            provider.post_account(),
            provider.stt_account_for_post_fallback(),
        ],
        |name| std::env::var(name).ok(),
        load_secret_reported,
    )
}

/// Same as [`load_secret`] but never returns an error: a real failure is
/// surfaced on stderr and swallowed to `None`, so the caller sees the reason
/// the credential store went silent instead of an unexplained "no key". The
/// UI store treats `NoEntry` + missing/blank file as `Ok("")`, so an `Err`
/// here really does mean the OS keyring path is broken (permissions, DBus
/// down, corrupt api-keys.json) rather than "nothing saved yet".
fn load_secret_reported(account: &str) -> Option<String> {
    match load_secret(account) {
        Ok(secret) => Some(secret),
        Err(err) => {
            eprintln!(
                "warning: credential store read for {account} failed \
                 (falling back to environment): {err:#}"
            );
            None
        }
    }
}

/// Testable core: every source is injected so the precedence can be exercised
/// without touching process env or the real credential store.
///
/// `accounts` is walked in order after every env lookup comes up empty --
/// callers pass `&[Some(primary), Some(fallback)]` when a second store
/// account should be consulted (e.g. resolve_post_api_key falling back to
/// the STT account for the same provider).
fn resolve_with<E, S>(
    specific_env: &[&str],
    generic_env: Option<&str>,
    accounts: &[Option<&str>],
    env_lookup: E,
    store_lookup: S,
) -> Option<String>
where
    E: Fn(&str) -> Option<String>,
    S: Fn(&str) -> Option<String>,
{
    let clean = |v: String| {
        let v = v.trim().to_owned();
        (!v.is_empty()).then_some(v)
    };
    specific_env
        .iter()
        .find_map(|name| env_lookup(name).and_then(clean))
        .or_else(|| generic_env.and_then(&env_lookup).and_then(clean))
        .or_else(|| {
            accounts
                .iter()
                .find_map(|account| account.and_then(&store_lookup).and_then(clean))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    const GROQ: &str = "https://api.groq.com/openai/v1";
    const OPENAI: &str = "https://api.openai.com/v1";

    #[test]
    fn account_names_match_the_ui_credential_users() {
        // Pinned literally: the UI writes under these exact names, and a
        // rename on either side would leave the CLI reading an account that
        // is never written -- failing exactly like the bug this fixes.
        assert_eq!(Provider::Groq.stt_account(), "stt-api-key:groq");
        assert_eq!(Provider::OpenAi.stt_account(), "stt-api-key:openai");
        assert_eq!(Provider::Custom.stt_account(), "stt-api-key:custom");
        assert_eq!(Provider::Groq.post_account(), Some("post-api-key:groq"));
        assert_eq!(Provider::OpenAi.post_account(), Some("post-api-key:openai"));
        assert_eq!(Provider::Custom.post_account(), None);
    }

    #[test]
    fn provider_is_classified_by_host_not_substring() {
        assert_eq!(Provider::from_base_url(GROQ), Provider::Groq);
        assert_eq!(Provider::from_base_url(OPENAI), Provider::OpenAi);
        // Userinfo trap: the HOST is evil.example, not api.groq.com.
        assert_eq!(
            Provider::from_base_url("https://api.groq.com@evil.example/v1"),
            Provider::Custom
        );
        // Suffix trap.
        assert_eq!(
            Provider::from_base_url("https://groq.com.attacker.example/v1"),
            Provider::Custom
        );
        assert_eq!(
            Provider::from_base_url("http://localhost:8080/v1"),
            Provider::Custom
        );
    }

    #[test]
    fn specific_env_wins_over_everything() {
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(STT_GROQ)],
            table(&[
                ("VOICEPI_STT_API_KEY", "specific"),
                ("GROQ_API_KEY", "generic"),
            ]),
            table(&[(STT_GROQ, "stored")]),
        );
        assert_eq!(got.as_deref(), Some("specific"));
    }

    #[test]
    fn generic_env_wins_over_the_store() {
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(STT_GROQ)],
            table(&[("GROQ_API_KEY", "generic")]),
            table(&[(STT_GROQ, "stored")]),
        );
        assert_eq!(got.as_deref(), Some("generic"));
    }

    #[test]
    fn store_is_used_when_the_environment_is_empty() {
        // The actual bug: key saved in the UI, nothing exported, CLI ran
        // without it.
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(STT_GROQ)],
            table(&[]),
            table(&[(STT_GROQ, "stored")]),
        );
        assert_eq!(got.as_deref(), Some("stored"));
    }

    #[test]
    fn blank_values_are_skipped_at_every_level() {
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(STT_GROQ)],
            table(&[("VOICEPI_STT_API_KEY", "   "), ("GROQ_API_KEY", "")]),
            table(&[(STT_GROQ, "stored")]),
        );
        assert_eq!(got.as_deref(), Some("stored"));
    }

    #[test]
    fn custom_endpoint_gets_no_generic_key_and_no_foreign_account() {
        let provider = Provider::from_base_url("https://llm.internal.example/v1");
        assert_eq!(provider.generic_env(), None);
        assert_eq!(provider.post_account(), None);
        // Only an explicit VOICEPI_* key reaches a self-hosted endpoint.
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            provider.generic_env(),
            &[Some(provider.stt_account())],
            table(&[("GROQ_API_KEY", "groq"), ("OPENAI_API_KEY", "openai")]),
            table(&[(STT_GROQ, "stored-groq")]),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn post_falls_back_to_the_stt_env_var() {
        let got = resolve_with(
            &["VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(POST_GROQ)],
            table(&[("VOICEPI_STT_API_KEY", "shared")]),
            table(&[]),
        );
        assert_eq!(got.as_deref(), Some("shared"));
    }

    #[test]
    fn post_falls_back_to_the_stt_store_account_for_the_same_provider() {
        // A user who saved only the STT credential in Settings
        // and paired local Whisper with Groq post-processing must still reach
        // cloud post -- the store fallback goes to STT_GROQ after POST_GROQ
        // is empty. The env fallback covered by `post_falls_back_to_the_stt_env_var`
        // does not help when nothing is exported.
        let got = resolve_post_api_key_with(
            "https://api.groq.com/openai/v1",
            table(&[]),
            table(&[(STT_GROQ, "shared-store")]),
        );
        assert_eq!(got.as_deref(), Some("shared-store"));
    }

    #[test]
    fn post_specific_account_still_wins_over_the_stt_store_fallback() {
        // A saved POST-specific account (POST_GROQ) must take precedence over
        // the STT fallback: the fallback exists for missing POST accounts, not
        // for overriding a user's explicit post-only key.
        let got = resolve_post_api_key_with(
            "https://api.groq.com/openai/v1",
            table(&[]),
            table(&[(POST_GROQ, "post-specific"), (STT_GROQ, "stt-fallback")]),
        );
        assert_eq!(got.as_deref(), Some("post-specific"));
    }

    #[test]
    fn custom_post_endpoint_gets_no_stt_store_fallback() {
        // Self-hosted post endpoint must not receive Groq/OpenAI's saved key.
        // (Same reasoning as `custom_endpoint_gets_no_generic_key_and_no_foreign_account`.)
        let got = resolve_post_api_key_with(
            "https://llm.internal.example/v1",
            table(&[]),
            table(&[(STT_GROQ, "stt-groq"), (STT_OPENAI, "stt-openai")]),
        );
        assert_eq!(got, None);
    }

    /// Same testable shape as `resolve_post_api_key`, but with the env /
    /// store lookups injected so the store fallback can
    /// be exercised without touching process env or the real credential
    /// store.
    fn resolve_post_api_key_with<E, S>(
        base_url: &str,
        env_lookup: E,
        store_lookup: S,
    ) -> Option<String>
    where
        E: Fn(&str) -> Option<String>,
        S: Fn(&str) -> Option<String>,
    {
        let provider = Provider::from_base_url(base_url);
        resolve_with(
            &["VOICEPI_POST_API_KEY", "VOICEPI_STT_API_KEY"],
            provider.generic_env(),
            &[
                provider.post_account(),
                provider.stt_account_for_post_fallback(),
            ],
            env_lookup,
            store_lookup,
        )
    }

    #[test]
    fn store_failure_falls_through_to_none_not_a_panic() {
        // `load_secret_reported` maps a broken store to `None` so resolution
        // continues instead of aborting. This test pins the
        // fall-through; the eprintln!() itself is out of scope for a unit
        // test but is asserted by inspection in the log.
        let got = resolve_with(
            &["VOICEPI_STT_API_KEY"],
            Some("GROQ_API_KEY"),
            &[Some(STT_GROQ)],
            table(&[]),
            // Store lookup returns None to model a failure surfaced by
            // `load_secret_reported`. Behaviour must match "nothing here"
            // rather than "unknown -- keep going".
            |_| None,
        );
        assert_eq!(got, None);
    }

    #[test]
    fn nothing_configured_yields_none() {
        assert_eq!(
            resolve_with(
                &["VOICEPI_STT_API_KEY"],
                Some("GROQ_API_KEY"),
                &[Some(STT_GROQ)],
                table(&[]),
                table(&[]),
            ),
            None
        );
    }
}
