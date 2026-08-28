//! Endpoint and metadata helpers for NVIDIA Nemotron's Riva gRPC API.
//!
//! Nemotron's hosted endpoint is a Riva gRPC service rather than an
//! OpenAI-compatible `/models` server. The actual API smoke request and
//! transcription adapter live in [`super::grpc_transcribe`]; this module keeps
//! endpoint classification, URL normalization, and function-id handling in one
//! place so the settings validator, test button, and runtime agree.

use anyhow::{anyhow, Result};
use http::Uri;

/// Function id used by NVIDIA's hosted Nemotron ASR Build endpoint.
///
/// A self-hosted Riva/NIM server does not need this metadata.  A custom
/// function id can be supplied as `?function-id=...` (or `function_id`) on the
/// endpoint URL; this keeps the key and function selection out of logs.
pub(crate) const NEMOTRON_NVCF_FUNCTION_ID: &str = "bb0837de-8c7b-481f-9ec8-ef5663e9c1fa";

pub(crate) const NEMOTRON_PROVIDER: &str = "nemotron 3.5 asr";
pub(crate) const NVCF_HOST: &str = "grpc.nvcf.nvidia.com";

/// Provider identifiers accepted at the runtime boundary. The settings UI
/// stores the short `nemotron` id, while older snapshots and status labels may
/// carry the human-readable name. Keeping this normalization in the protocol
/// module prevents either spelling from accidentally falling back to the
/// OpenAI-compatible HTTP client (which reports only `http: invalid format`
/// for NVIDIA's documented bare `host:port` endpoint).
pub(crate) fn is_nemotron_provider(provider: &str) -> bool {
    let provider = provider.trim().to_ascii_lowercase();
    provider == "nemotron"
        || provider == NEMOTRON_PROVIDER
        || provider == "nemotron 3.5 asr (nvidia nim)"
        || provider == "nvidia nemotron 3.5 asr"
}

/// Whether a Nemotron URL should use the Riva gRPC API.
///
/// Explicit `grpc://` URLs, NVIDIA's hosted gRPC hostname, port 50051, and a
/// `transport=grpc`/`protocol=grpc` query opt-in select this path without
/// changing the persisted `stt_backend=openai` compatibility value.
pub(crate) fn is_nemotron_grpc_endpoint(provider: &str, base_url: &str) -> bool {
    let provider = provider.trim();
    if !is_nemotron_provider(provider) {
        return false;
    }
    let lower = base_url.trim().to_ascii_lowercase();
    // Keep endpoint classification aligned with the parser used by the
    // request path.  In particular, `grpc://` without an authority must not
    // bypass the normal settings URL validation and fail only at test time.
    if endpoint_url(&lower).is_err() {
        return false;
    }
    lower.starts_with("grpc://")
        || authority_host(&lower).as_deref() == Some(NVCF_HOST)
        || authority_port(&lower) == Some(50051)
        || query_has_grpc_opt_in(&lower)
}

/// Whether a URL explicitly opts into the gRPC transport.
///
/// Port `50051` remains a useful signal once the user has selected the
/// Nemotron provider, but it is too ambiguous for provider inference from an
/// old config with no `stt_provider`: unrelated HTTP services commonly use
/// that port.  Config migration therefore only treats a `grpc://` scheme or a
/// transport/protocol query opt-in as an explicit gRPC declaration (the
/// hosted NVIDIA hostname is handled separately by
/// [`is_hosted_nemotron_endpoint`]).
pub(crate) fn has_explicit_grpc_transport(base_url: &str) -> bool {
    let lower = base_url.trim().to_ascii_lowercase();
    lower.starts_with("grpc://") || query_has_grpc_opt_in(&lower)
}

pub(crate) fn remaining_timeout(deadline: tokio::time::Instant) -> std::time::Duration {
    // `timeout` treats a zero duration as an immediate deadline. Keep a tiny
    // positive floor to avoid platform timer rounding turning an expired
    // request into an unbounded future poll.
    deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .max(std::time::Duration::from_millis(1))
}

pub(crate) fn endpoint_url(base_url: &str) -> Result<(String, bool)> {
    let raw = base_url.trim();
    if raw.is_empty() {
        return Err(anyhow!("Nemotron gRPC endpoint is empty"));
    }
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("grpc") => ("http", rest),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => ("http", rest),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => ("https", rest),
        _ => {
            // The NVIDIA quick-start shows `grpc.nvcf.nvidia.com:443` without
            // a URI scheme. Treat a bare endpoint as TLS, which is safe for
            // the hosted service and gives a useful error for malformed input.
            ("https", raw)
        }
    };
    let rest = rest.trim_end_matches('/');
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| anyhow!("Nemotron gRPC endpoint has no host"))?;
    let uri = format!("{scheme}://{authority}")
        .parse::<Uri>()
        .map_err(|err| anyhow!("invalid Nemotron gRPC endpoint: {err}"))?;
    Ok((
        uri.to_string().trim_end_matches('/').to_owned(),
        scheme == "https",
    ))
}

pub(crate) fn function_id(base_url: &str) -> Option<String> {
    if let Some(value) = query_function_id(base_url) {
        return Some(value);
    }
    (authority_host(base_url).as_deref() == Some(NVCF_HOST))
        .then(|| NEMOTRON_NVCF_FUNCTION_ID.to_owned())
}

/// Whether `base_url` names NVIDIA's hosted NVCF gateway. This intentionally
/// checks the parsed authority rather than a substring, so a URL such as
/// `grpc.nvcf.nvidia.com.attacker.example` cannot inherit hosted behaviour.
pub(crate) fn is_hosted_nemotron_endpoint(base_url: &str) -> bool {
    authority_host(base_url).as_deref() == Some(NVCF_HOST)
}

/// Whether the URL explicitly selects an NVCF function instead of the public
/// Build function. The function id is metadata, not part of the gRPC path, so
/// retaining it in the settings URL is the least surprising user-facing
/// configuration (`...?function-id=<id>`).
pub(crate) fn has_custom_function_id(base_url: &str) -> bool {
    query_function_id(base_url)
        .is_some_and(|id| !id.eq_ignore_ascii_case(NEMOTRON_NVCF_FUNCTION_ID))
}

/// Return a canonical value for the Speech-tab URL field.
///
/// Bare `grpc.nvcf.nvidia.com:443` is accepted by the CLI for compatibility,
/// but storing an explicit `https://` makes it clear that hosted traffic is
/// TLS. A local Riva port is stored as `grpc://` so it cannot accidentally be
/// sent through the OpenAI-compatible HTTP client.
pub(crate) fn canonical_nemotron_endpoint(base_url: &str) -> String {
    let raw = base_url.trim();
    if raw.is_empty() {
        return String::new();
    }
    let without_scheme = raw
        .split_once("://")
        .map_or(raw, |(_, remainder)| remainder);
    if is_hosted_nemotron_endpoint(raw) {
        format!("https://{}", without_scheme.trim_end_matches('/'))
    } else if !raw.contains("://") && authority_port(raw) == Some(50051) {
        format!("grpc://{}", without_scheme.trim_end_matches('/'))
    } else {
        raw.trim_end_matches('/').to_owned()
    }
}

/// Migrate endpoint values written by older Nemotron builds to the native
/// Riva transport.  The old NIM HTTP/WebSocket port (`9000`) and inherited
/// OpenAI/Groq defaults are not valid gRPC targets; an explicitly selected
/// Nemotron provider should start against the local Riva port instead.
pub(crate) fn migrate_nemotron_endpoint(base_url: &str, default_base_url: &str) -> String {
    let trimmed = base_url.trim();
    let inherited_default = [
        default_base_url,
        "https://api.groq.com/openai/v1",
        "http://localhost:8000/v1",
    ]
    .iter()
    .any(|value| trimmed.eq_ignore_ascii_case(value));
    let legacy_http = trimmed
        .trim_end_matches('/')
        .eq_ignore_ascii_case("http://localhost:9000/v1")
        || trimmed
            .trim_end_matches('/')
            .eq_ignore_ascii_case("http://localhost:9000");
    if inherited_default || legacy_http {
        "grpc://localhost:50051".to_owned()
    } else {
        canonical_nemotron_endpoint(trimmed)
    }
}

fn query_function_id(base_url: &str) -> Option<String> {
    let (_, query) = base_url.split_once('?')?;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (matches!(
            key.trim().to_ascii_lowercase().as_str(),
            "function-id" | "function_id"
        ) && !value.trim().is_empty())
        .then(|| value.trim().to_owned())
    })
}

fn query_has_grpc_opt_in(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else {
        return false;
    };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .any(|(key, value)| {
            matches!(key.trim(), "grpc" | "transport" | "protocol") && value.trim() == "1"
                || matches!(key.trim(), "transport" | "protocol")
                    && value.trim().eq_ignore_ascii_case("grpc")
        })
}

fn authority_port(url: &str) -> Option<u16> {
    let authority = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?;
    if let Some(ipv6) = authority.strip_prefix('[') {
        return ipv6
            .split_once(']')
            .and_then(|(_, rest)| rest.strip_prefix(':'))
            .and_then(|port| port.parse().ok());
    }
    authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

pub(crate) fn authority_host(url: &str) -> Option<String> {
    let authority = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?;
    let host = if let Some(ipv6) = authority.strip_prefix('[') {
        ipv6.split(']').next().unwrap_or("")
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, port)| {
                if port.chars().all(|character| character.is_ascii_digit()) {
                    host
                } else {
                    authority
                }
            })
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_nemotron_endpoint_selects_grpc_and_default_function_id() {
        let endpoint = "https://grpc.nvcf.nvidia.com:443";
        assert!(is_nemotron_grpc_endpoint(NEMOTRON_PROVIDER, endpoint));
        assert_eq!(
            function_id(endpoint).as_deref(),
            Some(NEMOTRON_NVCF_FUNCTION_ID)
        );
        assert!(is_hosted_nemotron_endpoint(endpoint));
        assert!(!has_custom_function_id(endpoint));
    }

    #[test]
    fn human_readable_nemotron_provider_labels_select_grpc() {
        for provider in [
            "Nemotron 3.5 ASR",
            "Nemotron 3.5 ASR (NVIDIA NIM)",
            "NVIDIA Nemotron 3.5 ASR",
        ] {
            assert!(is_nemotron_grpc_endpoint(
                provider,
                "grpc.nvcf.nvidia.com:443"
            ));
        }
    }

    #[test]
    fn legacy_local_http_nemotron_endpoint_is_not_grpc() {
        assert!(!is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "http://localhost:9000/v1"
        ));
    }

    #[test]
    fn explicit_grpc_schemes_and_port_are_detected() {
        assert!(is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "grpc://localhost:50051"
        ));
        assert!(is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "http://127.0.0.1:50051"
        ));
        assert!(!is_nemotron_grpc_endpoint(
            "OpenAI",
            "https://grpc.nvcf.nvidia.com:443"
        ));
        assert!(!is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "https://grpc.nvcf.nvidia.com.attacker.example:443"
        ));
        assert!(!is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "https://grpc.nvcf.nvidia.com@attacker.example:443"
        ));
        assert!(!is_nemotron_grpc_endpoint(NEMOTRON_PROVIDER, "grpc://"));
        assert!(!is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "grpc://?transport=grpc"
        ));
        assert!(is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "GRPC://localhost:50051"
        ));
        assert!(is_nemotron_grpc_endpoint(
            NEMOTRON_PROVIDER,
            "HTTPS://GRPC.NVCF.NVIDIA.COM:443"
        ));
        assert!(is_nemotron_grpc_endpoint(
            "nemotron",
            "https://grpc.nvcf.nvidia.com:443"
        ));
    }

    #[test]
    fn endpoint_normalization_supports_bare_host_and_grpc_scheme() {
        assert_eq!(
            endpoint_url("grpc.nvcf.nvidia.com:443").unwrap(),
            ("https://grpc.nvcf.nvidia.com:443".to_owned(), true)
        );
        assert_eq!(
            endpoint_url("grpc://localhost:50051/v1").unwrap(),
            ("http://localhost:50051".to_owned(), false)
        );
        assert_eq!(
            endpoint_url("GRPC://LOCALHOST:50051/v1").unwrap(),
            ("http://LOCALHOST:50051".to_owned(), false)
        );
        assert_eq!(
            endpoint_url("HTTPS://GRPC.NVCF.NVIDIA.COM:443").unwrap(),
            ("https://GRPC.NVCF.NVIDIA.COM:443".to_owned(), true)
        );
    }

    #[test]
    fn custom_function_id_query_overrides_hosted_default() {
        assert_eq!(
            function_id("https://grpc.nvcf.nvidia.com:443?function_id=custom-id").as_deref(),
            Some("custom-id")
        );
        assert!(has_custom_function_id(
            "https://grpc.nvcf.nvidia.com:443?function_id=custom-id"
        ));
        assert_eq!(
            function_id("https://grpc.nvcf.nvidia.com:443?function-id=custom-id").as_deref(),
            Some("custom-id")
        );
    }

    #[test]
    fn endpoint_canonicalization_makes_transport_explicit() {
        assert_eq!(
            canonical_nemotron_endpoint("grpc.nvcf.nvidia.com:443"),
            "https://grpc.nvcf.nvidia.com:443"
        );
        assert_eq!(
            canonical_nemotron_endpoint("localhost:50051/"),
            "grpc://localhost:50051"
        );
        assert_eq!(
            canonical_nemotron_endpoint("grpc.nvcf.nvidia.com:443?function-id=multi-function"),
            "https://grpc.nvcf.nvidia.com:443?function-id=multi-function"
        );
        assert_eq!(
            canonical_nemotron_endpoint("https://riva.example.com:50051/"),
            "https://riva.example.com:50051"
        );
    }

    #[test]
    fn public_function_id_is_not_a_multilingual_override() {
        let hosted =
            format!("https://grpc.nvcf.nvidia.com:443?function-id={NEMOTRON_NVCF_FUNCTION_ID}");
        assert!(!has_custom_function_id(&hosted));
        assert!(has_custom_function_id(
            "https://grpc.nvcf.nvidia.com:443?function-id=custom-multilingual-id"
        ));
    }

    #[test]
    fn explicit_grpc_transport_is_required_for_ambiguous_port_inference() {
        assert!(!has_explicit_grpc_transport(
            "https://internal.example:50051"
        ));
        assert!(has_explicit_grpc_transport("grpc://internal.example:50051"));
        assert!(has_explicit_grpc_transport(
            "https://internal.example:443?transport=grpc"
        ));
    }

    #[test]
    fn legacy_nemotron_endpoint_migration_is_shared_by_loaders() {
        assert_eq!(
            migrate_nemotron_endpoint("http://localhost:9000/v1", "https://api.openai.com/v1"),
            "grpc://localhost:50051"
        );
        assert_eq!(
            migrate_nemotron_endpoint("https://api.openai.com/v1", "https://api.openai.com/v1"),
            "grpc://localhost:50051"
        );
        assert_eq!(
            migrate_nemotron_endpoint(
                "https://riva.example.com:50051/",
                "https://api.openai.com/v1"
            ),
            "https://riva.example.com:50051"
        );
    }
}
