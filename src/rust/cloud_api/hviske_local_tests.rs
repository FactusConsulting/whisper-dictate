use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::{transcribe_at, CloudTranscriptionResult, LocalModel, AGENT, BASE_URL};

const CORRECT_HEALTH: &str = r#"{"model":"syvai/hviske-v6","device":"cuda"}"#;
const CORRECT_TRANSCRIPT: &str =
    r#"{"text":" hej ","model":"syvai/hviske-v6","device":"cuda","language":"en"}"#;
const HEALTH_PATH: &str = "/wd-hviske/v1/health";
const TRANSCRIBE_PATH: &str = "/wd-hviske/v1/transcribe";

struct Request {
    headers: String,
    body: Vec<u8>,
}

#[derive(Clone, Copy)]
struct Response {
    status: &'static str,
    extra_headers: &'static str,
    body: &'static str,
}

impl Response {
    const fn json(body: &'static str) -> Self {
        Self {
            status: "200 OK",
            extra_headers: "",
            body,
        }
    }
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "expected loopback request");
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn serve(listener: &TcpListener, response: Response) -> Request {
    let mut stream = accept(listener);
    crate::test_http_stream::configure(&stream);
    let mut reader = BufReader::new(&mut stream);
    let mut headers = String::new();
    loop {
        assert!(reader.read_line(&mut headers).unwrap() > 0);
        if headers.ends_with("\r\n\r\n") {
            break;
        }
    }
    let headers = headers.to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |n| n.trim().parse().unwrap());
    assert!(length < 100_000);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let Response {
        status,
        extra_headers,
        body: response_body,
    } = response;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{response_body}",
        response_body.len(),
    )
    .unwrap();
    Request { headers, body }
}

fn round_trip(responses: Vec<Response>) -> (Result<CloudTranscriptionResult>, Vec<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = listener.try_clone().unwrap();
    let server = thread::spawn(move || {
        responses
            .into_iter()
            .map(|response| serve(&worker, response))
            .collect()
    });
    let result = transcribe_at(
        &format!("{base}{HEALTH_PATH}"),
        &format!("{base}{TRANSCRIBE_PATH}"),
        LocalModel::V6,
        &wav(),
        1_000,
    );
    let requests = server.join().unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock,
        "client made an unexpected extra request"
    );
    (result, requests)
}

fn wav() -> Vec<u8> {
    crate::dictate::backends::cloud_transcribe::encode_wav_mono_16bit(&[0.2; 32], 16_000).unwrap()
}

fn assert_health_request(request: &Request) {
    assert!(request.headers.starts_with("get /wd-hviske/v1/health "));
    assert!(!request.headers.contains("authorization:"));
    assert!(request.body.is_empty());
}

#[test]
fn wav_is_posted_only_after_verified_cuda_health() {
    let (result, requests) = round_trip(vec![
        Response::json(CORRECT_HEALTH),
        Response::json(CORRECT_TRANSCRIPT),
    ]);
    assert_health_request(&requests[0]);
    let request = &requests[1];
    assert!(request
        .headers
        .starts_with("post /wd-hviske/v1/transcribe "));
    assert!(request.headers.contains("content-type: audio/wav\r\n"));
    assert!(request
        .headers
        .contains("x-wd-hviske-model: syvai/hviske-v6\r\n"));
    assert!(!request.headers.contains("authorization:"));
    assert_eq!(request.body, wav());
    let transcript = result.unwrap();
    assert_eq!(transcript.text, "hej");
    assert_eq!(transcript.language.as_deref(), Some("da"));
}

#[test]
fn unready_mismatched_or_redirected_health_never_receives_audio() {
    for (health, reason) in [
        (
            Response {
                status: "503 Service Unavailable",
                extra_headers: "",
                body: CORRECT_HEALTH,
            },
            "503",
        ),
        (
            Response {
                status: "307 Temporary Redirect",
                extra_headers: "Location: /should-not-follow\r\n",
                body: CORRECT_HEALTH,
            },
            "307",
        ),
        (
            Response::json(r#"{"model":"syvai/hviske-v5.3","device":"cuda"}"#),
            "expected model",
        ),
        (
            Response::json(r#"{"model":"syvai/hviske-v6","device":"cpu"}"#),
            "expected model",
        ),
        (Response::json("{}"), "invalid JSON"),
    ] {
        let (result, requests) = round_trip(vec![health]);
        assert_health_request(&requests[0]);
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("not ready") && error.contains(reason),
            "{error}"
        );
    }
}

#[test]
fn wrong_transcript_identity_or_redirect_is_rejected_after_health() {
    for (transcript, reason) in [
        (
            Response::json(r#"{"text":"wrong","model":"syvai/hviske-v5.3","device":"cuda"}"#),
            "expected model",
        ),
        (
            Response::json(r#"{"text":"wrong","model":"syvai/hviske-v6","device":"cpu"}"#),
            "expected model",
        ),
        (
            Response {
                status: "307 Temporary Redirect",
                extra_headers: "Location: /should-not-follow\r\n",
                body: CORRECT_TRANSCRIPT,
            },
            "307",
        ),
    ] {
        let (result, requests) = round_trip(vec![Response::json(CORRECT_HEALTH), transcript]);
        assert_health_request(&requests[0]);
        assert_eq!(requests[1].body, wav());
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains(reason), "{error}");
    }
}

#[test]
fn agent_has_no_environment_proxy() {
    assert!(AGENT.config().proxy().is_none());
    assert_eq!(AGENT.config().max_redirects(), 0);
}

#[test]
fn local_hviske_check_ignores_remote_url_and_credentials() {
    use crate::cloud_api::CloudApiCheck;
    use crate::config::AppSettings;

    let mut settings = AppSettings {
        stt_backend: "openai".to_owned(),
        stt_provider: "custom".to_owned(),
        stt_base_url: "https://api.groq.com/openai/v1".to_owned(),
        stt_model: LocalModel::V53.alias().to_owned(),
        lang: "en".to_owned(),
        local_only: true,
        ..AppSettings::default()
    };
    for key in ["", "unused-cloud-key"] {
        let check = CloudApiCheck::from_settings(&settings, key).unwrap();
        assert!(check.api_key.is_empty());
        assert_eq!(check.base_url, BASE_URL);
        assert_eq!(check.language.as_deref(), Some("da"));
        assert!(check.operation().contains("CUDA readiness"));
        assert!(!check.uses_grpc() && !check.uses_in_process());
    }
    let mut check = CloudApiCheck::from_settings(&settings, "unused-cloud-key").unwrap();
    check.provider = "groq".to_owned();
    let error = crate::cloud_api::check_cloud_api(&check).unwrap_err();
    assert!(error.to_string().contains("Custom STT provider"));

    settings.stt_provider = "groq".to_owned();
    let error = CloudApiCheck::from_settings(&settings, "unused-cloud-key").unwrap_err();
    assert!(error.to_string().contains("Custom STT provider"));
}

#[test]
fn aliases_do_not_capture_hosted_model_ids() {
    for model in [LocalModel::V53, LocalModel::V6] {
        assert_eq!(LocalModel::from_alias(model.alias()), Some(model));
        assert_eq!(LocalModel::from_alias(model.model_id()), None);
    }
}

#[test]
fn invalid_wav_is_refused_before_contact() {
    let wrong_sample_rate =
        crate::dictate::backends::cloud_transcribe::encode_wav_mono_16bit(&[0.0; 32], 8_000)
            .unwrap();
    let error = transcribe_at(
        "invalid-url",
        "invalid-url",
        LocalModel::V6,
        &wrong_sample_rate,
        1000,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("mono 16 kHz signed 16-bit PCM WAV"),
        "{error}"
    );
}

#[test]
fn local_alias_routes_before_hosted_credential_and_endpoint_checks() {
    use crate::cloud_api::{
        cloud_transcribe, cloud_transcribe_for_provider, CloudTranscriptionRequest,
    };

    let base_url = "https://api.groq.com/openai/v1";
    let model = LocalModel::V53.alias();
    let invalid_audio = b"not a WAV";
    let legacy = cloud_transcribe(base_url, "", model, invalid_audio, None, None, 1_000);
    let configured = cloud_transcribe_for_provider(
        "custom",
        CloudTranscriptionRequest::new(base_url, "", model, invalid_audio, None, None, 1_000),
    );
    for result in [legacy, configured] {
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("local Hviske requires valid WAV audio"));
    }

    let err = cloud_transcribe_for_provider(
        "groq",
        CloudTranscriptionRequest::new(base_url, "", model, invalid_audio, None, None, 1_000),
    )
    .unwrap_err();
    assert!(err.to_string().contains("Custom STT provider"));
}
