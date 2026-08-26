//! Small Riva gRPC probe for NVIDIA Nemotron API checks.
//!
//! Nemotron's hosted endpoint is a Riva gRPC service rather than an
//! OpenAI-compatible `/models` server.  Keep this client deliberately narrow:
//! the application still transcribes through the existing HTTP-compatible
//! endpoint, while the Speech-tab API check uses the Riva configuration RPC to
//! verify connectivity, credentials, and the selected service.

use anyhow::{anyhow, Context, Result};
use http::{uri::PathAndQuery, Uri};
use prost::Message;
use std::time::Duration;
use tonic::{
    client::Grpc,
    metadata::MetadataValue,
    transport::{ClientTlsConfig, Endpoint},
    Request,
};
use tonic_prost::ProstCodec;

/// Function id used by NVIDIA's hosted Nemotron ASR Build endpoint.
///
/// A self-hosted Riva/NIM server does not need this metadata.  A custom
/// function id can be supplied as `?function-id=...` (or `function_id`) on the
/// endpoint URL; this keeps the key and function selection out of logs.
pub(crate) const NEMOTRON_NVCF_FUNCTION_ID: &str = "bb0837de-8c7b-481f-9ec8-ef5663e9c1fa";

const NEMOTRON_PROVIDER: &str = "nemotron 3.5 asr";
const NVCF_HOST: &str = "grpc.nvcf.nvidia.com";
const GET_CONFIG_PATH: &str =
    "/nvidia.riva.asr.RivaSpeechRecognition/GetRivaSpeechRecognitionConfig";

/// Whether a Nemotron URL should use the Riva gRPC API check.
///
/// The normal local NIM URL (`http://localhost:9000/v1`) remains on the
/// OpenAI-compatible HTTP check.  Explicit `grpc://` URLs, NVIDIA's hosted
/// gRPC hostname, port 50051, and a `transport=grpc`/`protocol=grpc` query
/// opt-in select this path without changing the transcription backend.
pub(crate) fn is_nemotron_grpc_endpoint(provider: &str, base_url: &str) -> bool {
    if !provider.trim().eq_ignore_ascii_case(NEMOTRON_PROVIDER) {
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

/// Probe the Riva config RPC and return the model names advertised by it.
pub(crate) fn check_nemotron_grpc(
    base_url: &str,
    api_key: &str,
    timeout_ms: u64,
) -> Result<Vec<String>> {
    let timeout = Duration::from_millis(timeout_ms.max(1_000));
    let (endpoint_url, tls) = endpoint_url(base_url)?;
    let function_id = function_id(base_url);
    let api_key = api_key.trim().to_owned();
    let endpoint_host = endpoint_url.clone();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not create the Nemotron gRPC runtime")?;
    runtime.block_on(async move {
        let deadline = tokio::time::Instant::now() + timeout;
        let endpoint = Endpoint::from_shared(endpoint_url.clone())
            .map_err(|err| anyhow!("invalid Nemotron gRPC endpoint: {err}"))?
            .connect_timeout(timeout);
        let endpoint = if tls {
            endpoint
                .tls_config(ClientTlsConfig::new().with_enabled_roots())
                .map_err(|err| anyhow!("could not configure Nemotron gRPC TLS: {err}"))?
        } else {
            endpoint
        };
        let connect_timeout = remaining_timeout(deadline);
        let channel = tokio::time::timeout(connect_timeout, endpoint.connect())
            .await
            .map_err(|_| timeout_error(&endpoint_host, timeout))?
            .map_err(|err| anyhow!("Nemotron gRPC connection failed: {err}"))?;

        let mut request = Request::new(RivaSpeechRecognitionConfigRequest::default());
        if !api_key.is_empty() {
            let value = MetadataValue::try_from(format!("Bearer {api_key}"))
                .map_err(|_| anyhow!("Nemotron gRPC API key could not be encoded"))?;
            request.metadata_mut().insert("authorization", value);
        }
        if let Some(function_id) = function_id {
            let value = MetadataValue::try_from(function_id.as_str())
                .map_err(|_| anyhow!("Nemotron gRPC function id could not be encoded"))?;
            request.metadata_mut().insert("function-id", value);
        }

        let mut grpc = Grpc::new(channel);
        let response =
            tokio::time::timeout(
                remaining_timeout(deadline),
                grpc.unary(
                    request,
                    PathAndQuery::from_static(GET_CONFIG_PATH),
                    ProstCodec::<
                        RivaSpeechRecognitionConfigRequest,
                        RivaSpeechRecognitionConfigResponse,
                    >::default(),
                ),
            )
            .await
            .map_err(|_| timeout_error(&endpoint_host, timeout))?
            .map_err(|status| anyhow!("Nemotron gRPC API check failed: {status}"))?;

        Ok(response
            .into_inner()
            .model_config
            .into_iter()
            .filter_map(|config| {
                let model = config.model_name.trim().to_owned();
                (!model.is_empty()).then_some(model)
            })
            .collect())
    })
}

fn remaining_timeout(deadline: tokio::time::Instant) -> Duration {
    // `timeout` treats a zero duration as an immediate deadline. Keep a tiny
    // positive floor to avoid platform timer rounding turning an expired
    // request into an unbounded future poll.
    deadline
        .saturating_duration_since(tokio::time::Instant::now())
        .max(Duration::from_millis(1))
}

fn timeout_error(endpoint: &str, timeout: Duration) -> anyhow::Error {
    anyhow!(
        "Nemotron gRPC API check timed out after {} ms ({endpoint})",
        timeout.as_millis()
    )
}

fn endpoint_url(base_url: &str) -> Result<(String, bool)> {
    let raw = base_url.trim();
    if raw.is_empty() {
        return Err(anyhow!("Nemotron gRPC endpoint is empty"));
    }
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("grpc://") {
        ("http", rest)
    } else if let Some(rest) = raw.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = raw.strip_prefix("https://") {
        ("https", rest)
    } else {
        // The NVIDIA quick-start shows `grpc.nvcf.nvidia.com:443` without a
        // URI scheme. Treat a bare endpoint as TLS, which is safe for the
        // hosted service and gives a useful error for malformed input.
        ("https", raw)
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

fn function_id(base_url: &str) -> Option<String> {
    if let Some((_, query)) = base_url.split_once('?') {
        let query = query.split('#').next().unwrap_or(query);
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if matches!(
                key.trim().to_ascii_lowercase().as_str(),
                "function-id" | "function_id"
            ) && !value.trim().is_empty()
            {
                return Some(value.trim().to_owned());
            }
        }
    }
    (authority_host(base_url).as_deref() == Some(NVCF_HOST))
        .then(|| NEMOTRON_NVCF_FUNCTION_ID.to_owned())
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

fn authority_host(url: &str) -> Option<String> {
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

/// Minimal subset of `riva_asr.proto` required for the configuration RPC.
/// Keeping the messages local avoids a build-time `protoc` dependency and the
/// large, unrelated Riva TTS/NMT generated surface.
#[derive(Clone, PartialEq, Message)]
struct RivaSpeechRecognitionConfigRequest {
    #[prost(string, tag = "1")]
    model_name: String,
}

#[derive(Clone, PartialEq, Message)]
struct RivaSpeechRecognitionConfigResponse {
    #[prost(message, repeated, tag = "1")]
    model_config: Vec<RivaModelConfig>,
}

#[derive(Clone, PartialEq, Message)]
struct RivaModelConfig {
    #[prost(string, tag = "1")]
    model_name: String,
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
    }

    #[test]
    fn local_http_nemotron_endpoint_stays_openai_compatible() {
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
    }

    #[test]
    fn custom_function_id_query_overrides_hosted_default() {
        assert_eq!(
            function_id("https://grpc.nvcf.nvidia.com:443?function_id=custom-id").as_deref(),
            Some("custom-id")
        );
    }

    #[test]
    fn config_probe_request_defaults_to_all_models() {
        let request = RivaSpeechRecognitionConfigRequest::default();
        let mut bytes = Vec::new();
        request.encode(&mut bytes).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn config_response_decodes_model_names() {
        let response = RivaSpeechRecognitionConfigResponse {
            model_config: vec![RivaModelConfig {
                model_name: "nemotron-asr-streaming".to_owned(),
            }],
        };
        let mut bytes = Vec::new();
        response.encode(&mut bytes).unwrap();
        let decoded = RivaSpeechRecognitionConfigResponse::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.model_config[0].model_name, "nemotron-asr-streaming");
    }
}
