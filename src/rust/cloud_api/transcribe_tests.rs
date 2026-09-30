use super::*;

#[test]
fn successful_transcription_requires_an_object_with_string_text() {
    for body in [
        serde_json::json!({}),
        serde_json::json!({"text": null}),
        serde_json::json!({"text": 123}),
        serde_json::json!({"text": ["hello"]}),
        serde_json::json!({"message": "quota exhausted"}),
        serde_json::json!([{"text": "hello"}]),
        serde_json::json!("hello"),
        serde_json::Value::Null,
    ] {
        let error = parse_transcription_response(&body).unwrap_err().to_string();
        assert!(error.contains("string text field"));
        assert!(
            !error.contains("quota exhausted"),
            "response payload must not be logged"
        );
    }
}

#[test]
fn legitimate_empty_transcriptions_and_optional_language_remain_valid() {
    for text in ["", "  ", "hello"] {
        let result = parse_transcription_response(&serde_json::json!({"text": text})).unwrap();
        assert_eq!(result.text, text.trim());
        assert_eq!(result.language, None);
    }
    let result =
        parse_transcription_response(&serde_json::json!({"text": " hej ", "language": "da"}))
            .unwrap();
    assert_eq!(result.text, "hej");
    assert_eq!(result.language.as_deref(), Some("da"));
}

#[test]
fn actual_http_success_with_malformed_body_returns_a_backend_error() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        crate::test_http_stream::configure(&stream);
        let mut reader = BufReader::new(&mut stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
            if line == "\r\n" {
                break;
            }
        }
        assert!(length < 1_000_000);
        reader.read_exact(&mut vec![0; length]).unwrap();
        let body = r#"{"message":"provider returned the wrong envelope"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let config = crate::dictate::CloudTranscribeConfig {
        base_url,
        api_key: String::new(),
        model: "test-model".to_owned(),
        timeout_ms: 2_000,
        language: None,
        prompt: None,
    };
    let backend = crate::dictate::CloudTranscribeBackend::new(config).with_transcription_guards(
        crate::dictate::backends::hallucination::TranscriptionGuards::from_lookup(|_| None),
    );
    let pcm: Vec<_> = [0.001_f32, 0.5, 0.001, 0.5, 0.001, 0.5]
        .into_iter()
        .flat_map(|amp| std::iter::repeat_n(amp, 480))
        .collect();
    use crate::dictate::TranscribeBackend;
    let error = backend.transcribe(&pcm, 16_000).unwrap_err().to_string();
    server.join().unwrap();
    assert!(error.contains("string text field"), "{error}");
    assert!(!error.contains("wrong envelope"), "{error}");
}
