//! Exercise scoped runtime credentials through the real HTTP backend.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn local_server() -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "backend did not contact the server"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        crate::test_http_stream::configure(&stream);
        let mut reader = BufReader::new(&mut stream);
        let mut headers = String::new();
        let mut body_len = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            headers.push_str(&line);
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                body_len = value.trim().parse::<usize>().unwrap();
            }
            if line == "\r\n" {
                break;
            }
            assert!(headers.len() < 16_384);
        }
        assert!(body_len < 1_000_000);
        reader.read_exact(&mut vec![0; body_len]).unwrap();
        let body = r#"{"text":"hello"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        headers
    });
    (base_url, server)
}

#[test]
fn runtime_snapshot_generic_keys_are_not_sent_to_unrelated_http_endpoint() {
    let (base_url, server) = local_server();
    let runtime =
        crate::runtime::settings_snapshot::RuntimeSettingsSnapshot::from_pairs_with_ambient(
            [
                (STT_BASE_URL_ENV.to_owned(), base_url),
                (STT_MODEL_ENV.to_owned(), "test-model".to_owned()),
            ],
            |name| match name {
                "OPENAI_API_KEY" => Some("synthetic-openai-key".to_owned()),
                "GROQ_API_KEY" => Some("synthetic-groq-key".to_owned()),
                _ => None,
            },
        )
        .unwrap();
    let config = CloudTranscribeConfig::from_env_with_provider(
        |name| runtime.value(name).map(str::to_owned),
        "custom",
    );
    let backend = CloudTranscribeBackend::new_with_provider(config, "custom")
        .with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));
    let pcm: Vec<_> = [0.001_f32, 0.5, 0.001, 0.5, 0.001, 0.5]
        .into_iter()
        .flat_map(|amp| std::iter::repeat_n(amp, 480))
        .collect();
    assert_eq!(backend.transcribe(&pcm, 16_000).unwrap().text, "hello");
    let headers = server.join().unwrap().to_ascii_lowercase();
    assert!(headers.starts_with("post /v1/audio/transcriptions "));
    assert!(
        !headers.contains("authorization:"),
        "generic key leaked: {headers}"
    );
}
