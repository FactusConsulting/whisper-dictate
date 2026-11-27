//! Exercise scoped runtime credentials through the real HTTP backend.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

fn local_server(response_body: &'static str) -> (String, thread::JoinHandle<(String, Vec<u8>)>) {
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
        let mut request_body = vec![0; body_len];
        reader.read_exact(&mut request_body).unwrap();
        let body = response_body;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        (headers, request_body)
    });
    (base_url, server)
}

#[test]
fn runtime_snapshot_generic_keys_are_not_sent_to_unrelated_http_endpoint() {
    let (base_url, server) = local_server(r#"{"text":"hello"}"#);
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
    let headers = server.join().unwrap().0.to_ascii_lowercase();
    assert!(headers.starts_with("post /v1/audio/transcriptions "));
    assert!(
        !headers.contains("authorization:"),
        "generic key leaked: {headers}"
    );
}

#[test]
fn cloud_transcribe_keeps_pre_boost_metrics_on_text_empty_and_gate_results() {
    use crate::audio_dsp::{capture_metrics, StatusThresholds, FRAME_SAMPLES};

    let thresholds = StatusThresholds::default();
    let speech: Vec<_> = [0.001_f32, 0.02, 0.001, 0.02, 0.001, 0.02]
        .into_iter()
        .flat_map(|amp| std::iter::repeat_n(amp, FRAME_SAMPLES))
        .collect();
    let flat = vec![0.01_f32; 6 * FRAME_SAMPLES];
    let speech_metrics = capture_metrics(&speech, &thresholds);
    assert_eq!(speech_metrics.peak, f64::from(0.02_f32));
    assert!(speech_metrics.gain > 1.0);
    let flat_metrics = capture_metrics(&flat, &thresholds);
    assert_eq!(flat_metrics.input_status, "low_snr");

    for (body, text) in [(r#"{"text":"hello"}"#, "hello"), (r#"{"text":""}"#, "")] {
        let (base_url, server) = local_server(body);
        let config = CloudTranscribeConfig {
            base_url,
            api_key: String::new(),
            model: "test-model".to_owned(),
            timeout_ms: 5_000,
            language: None,
            prompt: None,
        };
        let backend = CloudTranscribeBackend::new_with_provider(config, "custom")
            .with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));

        let rejected = backend.transcribe(&flat, 16_000).unwrap();
        assert!(rejected.text.is_empty());
        assert!(rejected.gate.unwrap().contains("no speech contrast"));
        assert_eq!(rejected.audio_metrics.as_ref(), Some(&flat_metrics));

        let decoded = backend.transcribe(&speech, 16_000).unwrap();
        assert_eq!(decoded.text, text);
        assert!(decoded.gate.is_none());
        assert_eq!(decoded.audio_metrics.as_ref(), Some(&speech_metrics));
        server.join().unwrap();
    }
}

#[test]
fn hviske_suppresses_dictionary_bias_but_keeps_transcript_replacements() {
    use crate::dictionary::{Dictionary, Replacement, SessionDictionary};
    use crate::postprocess::settings_from_env_with;
    use crate::transcribe_file::{build_cloud_backend, transcribe_path, ConfiguredBackend};

    let directory = tempfile::tempdir().unwrap();
    let wav = directory.path().join("speech.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
    for amplitude in [32_i16, 16_384, 32, 16_384, 32, 16_384] {
        for _ in 0..480 {
            writer.write_sample(amplitude).unwrap();
        }
    }
    writer.finalize().unwrap();

    let dictionary = SessionDictionary {
        dictionary: Dictionary {
            terms: vec!["Personnavn".to_owned()],
            replacements: vec![Replacement {
                from: "hello".to_owned(),
                to: "hej verden".to_owned(),
            }],
        },
        max_terms: 1,
        max_chars: 40,
        enabled: true,
    };
    for model in [
        crate::cloud_api::HVISKE_V53,
        crate::cloud_api::HVISKE_V6,
        "other-model",
    ] {
        let (base_url, server) = local_server(r#"{"text":"hello"}"#);
        let config = CloudTranscribeConfig {
            base_url,
            model: model.to_owned(),
            language: Some("en".to_owned()),
            prompt: Some("Fixture vocabulary".to_owned()),
            timeout_ms: 2_000,
            api_key: String::new(),
        };
        let backend = build_cloud_backend(config, &dictionary, true, "custom")
            .unwrap()
            .with_transcription_guards(TranscriptionGuards::from_lookup(|_| None));
        let report = transcribe_path(
            &wav,
            ConfiguredBackend::Cloud,
            &backend,
            model,
            &dictionary,
            &mut settings_from_env_with(|_| None),
        )
        .unwrap();
        let (headers, body) = server.join().unwrap();
        let body = String::from_utf8_lossy(&body);

        assert!(headers.starts_with("POST /v1/audio/transcriptions "));
        assert!(body.contains(&format!("name=\"model\"\r\n\r\n{model}\r\n")));
        assert_eq!(report.raw_text, "hello");
        assert_eq!(report.text, "hej verden");
        assert_eq!(report.dictionary_replacements[0].count, 1);
        if model == "other-model" {
            assert!(body.contains("name=\"language\"\r\n\r\nen\r\n"));
            assert!(body
                .contains("name=\"prompt\"\r\n\r\nFixture vocabulary\nVocabulary: Personnavn\r\n"));
            assert_eq!(report.dictionary_terms, ["Personnavn"]);
        } else {
            assert!(body.contains("name=\"language\"\r\n\r\nda\r\n"));
            assert!(!body.contains("name=\"prompt\""));
            assert!(!body.contains("Personnavn"));
            assert!(report.dictionary_terms.is_empty());
            assert_eq!(report.language, "da");
        }
    }
}
