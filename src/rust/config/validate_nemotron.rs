//! Nemotron-specific settings validation.
//!
//! Keeping this provider guard separate from the general settings validator
//! makes the gRPC/profile contract easy to review without growing the common
//! validation module past the repository's file-size limit.

use anyhow::{anyhow, Result};

use super::settings::AppSettings;

impl AppSettings {
    /// The English-only Nemotron deployment cannot perform language
    /// identification and rejects non-English locale hints. Keep this guard
    /// at the settings boundary so the UI, CLI, and persisted config all fail
    /// with the same actionable message; the backend still has a legacy Auto
    /// fallback for snapshots created before this validation existed.
    pub(crate) fn validate_nemotron_profile_language(&self) -> Result<()> {
        // Keep this in lockstep with the runtime/provider labels accepted by
        // the gRPC adapter. Persisted settings normally use the short
        // `nemotron` id, but older runtime snapshots can carry the human
        // readable provider label.
        let provider_is_nemotron = crate::cloud_api::is_nemotron_provider(&self.stt_provider);
        // The selected provider is authoritative. A custom OpenAI-compatible
        // endpoint may intentionally expose a Nemotron-named model, but it
        // does not necessarily implement Nemotron's English-only profile
        // contract (and must stay on the generic HTTP path).
        if provider_is_nemotron
            && crate::dictate::backends::cloud_transcribe::
                nemotron_english_profile_requires_language(&self.stt_model, &self.lang)
        {
            return Err(anyhow!(
                "Nemotron English profile requires Language=English (en); choose English or switch to the Multilingual / Auto profile"
            ));
        }
        if !provider_is_nemotron {
            return Ok(());
        }

        // NIM's streaming API is gRPC on 50051. Older builds suggested the
        // HTTP port (`9000/v1`), which produces an opaque 415/invalid-format
        // failure because this client sends Riva protobuf messages. Reject it
        // at the settings boundary with the replacement URL.
        let legacy_http = self
            .stt_base_url
            .trim_end_matches('/')
            .eq_ignore_ascii_case("http://localhost:9000/v1")
            || self
                .stt_base_url
                .trim_end_matches('/')
                .eq_ignore_ascii_case("http://localhost:9000");
        if legacy_http {
            return Err(anyhow!(
                "Nemotron uses Riva gRPC; set stt_base_url to grpc://localhost:50051 (port 9000 is HTTP/WebSocket only)"
            ));
        }

        // Do not let a different HTTP URL through to the runtime. Nemotron
        // is not an OpenAI-compatible transcription service, so accepting an
        // arbitrary `http(s)://...` value here would only defer the failure to
        // request time (typically as `http: invalid format` or HTTP 415). The
        // endpoint classifier also accepts the documented bare hosted
        // authority and custom gRPC ports, while rejecting malformed URLs.
        if !crate::cloud_api::is_nemotron_grpc_endpoint("nemotron", self.stt_base_url.trim()) {
            return Err(anyhow!(
                "Nemotron requires a Riva gRPC endpoint; use grpc://localhost:50051 locally or https://grpc.nvcf.nvidia.com:443 for hosted NVCF"
            ));
        }

        if crate::dictate::backends::cloud_transcribe::is_nemotron_multilingual_model(
            &self.stt_model,
        ) {
            if !crate::dictate::backends::cloud_transcribe::is_nemotron_supported_language_hint(
                &self.lang,
            ) {
                return Err(anyhow!(
                    "Nemotron Multilingual profile supports Language=Auto or a supported locale (for example en, da, de, fr); got {:?}",
                    self.lang.trim()
                ));
            }
            // NVIDIA's public Build function currently exposes the English
            // profile. Selecting the multilingual model in the UI does not
            // change a hosted function's deployment; a user-owned NVCF
            // function id is required for hosted multilingual inference.
            if crate::cloud_api::is_hosted_nemotron_endpoint(&self.stt_base_url)
                && !crate::cloud_api::has_custom_function_id(&self.stt_base_url)
            {
                return Err(anyhow!(
                    "the public hosted Nemotron endpoint is English-only; add ?function-id=<your multilingual NVCF function id> or use grpc://localhost:50051 for the Multilingual / Auto profile"
                ));
            }
        }
        Ok(())
    }
}
