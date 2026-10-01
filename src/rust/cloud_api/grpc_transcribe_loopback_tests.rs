//! Real loopback HTTP/2 + protobuf tests, not acoustic quality benchmarks.
use super::super::*;
use crate::dictate::TranscribeBackend;
use std::{
    convert::Infallible,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tonic::{
    body::Body,
    codegen::{BoxFuture, Service},
    server::{NamedService, StreamingService},
    Response, Status,
};

type Responses =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<StreamingRecognizeResponse, Status>> + Send>>;

#[derive(Clone)]
struct RivaFixture {
    language: &'static str,
    fail: Arc<std::sync::atomic::AtomicBool>,
    requests: Arc<Mutex<Vec<(String, usize)>>>,
}

impl NamedService for RivaFixture {
    const NAME: &'static str = "nvidia.riva.asr.RivaSpeechRecognition";
}

impl Service<http::Request<Body>> for RivaFixture {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let fixture = self.clone();
        Box::pin(async move {
            assert_eq!(request.uri().path(), STREAMING_RECOGNIZE_PATH);
            let codec =
                ProstCodec::<StreamingRecognizeResponse, StreamingRecognizeRequest>::default();
            Ok(tonic::server::Grpc::new(codec)
                .streaming(fixture, request)
                .await)
        })
    }
}

impl StreamingService<StreamingRecognizeRequest> for RivaFixture {
    type Response = StreamingRecognizeResponse;
    type ResponseStream = Responses;
    type Future = BoxFuture<Response<Responses>, Status>;
    fn call(
        &mut self,
        request: Request<tonic::Streaming<StreamingRecognizeRequest>>,
    ) -> Self::Future {
        let fixture = self.clone();
        Box::pin(async move {
            let mut stream = request.into_inner();
            let first = stream
                .message()
                .await?
                .ok_or_else(|| Status::invalid_argument("missing config"))?;
            let Some(streaming_recognize_request::StreamingRequest::StreamingConfig(config)) =
                first.streaming_request
            else {
                return Err(Status::invalid_argument("first message must be config"));
            };
            let config = config
                .config
                .ok_or_else(|| Status::invalid_argument("missing recognition config"))?;
            assert_eq!(config.encoding, LINEAR_PCM_ENCODING);
            assert_eq!(config.sample_rate_hertz, 16_000);
            let mut bytes = 0;
            while let Some(message) = stream.message().await? {
                let Some(streaming_recognize_request::StreamingRequest::AudioContent(audio)) =
                    message.streaming_request
                else {
                    return Err(Status::invalid_argument("expected PCM chunk"));
                };
                bytes += audio.len();
            }
            fixture
                .requests
                .lock()
                .unwrap()
                .push((config.language_code, bytes));
            if fixture.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(Status::invalid_argument("fixture rejected audio format"));
            }
            let replies = [
                StreamingRecognizeResponse {
                    results: vec![StreamingRecognitionResult {
                        alternatives: vec![SpeechRecognitionAlternative {
                            transcript: "interim".to_owned(),
                            language_code: vec!["en-US".to_owned()],
                        }],
                        is_final: false,
                    }],
                },
                StreamingRecognizeResponse {
                    results: vec![StreamingRecognitionResult {
                        alternatives: vec![SpeechRecognitionAlternative {
                            transcript: "fixture final transcript".to_owned(),
                            language_code: vec![fixture.language.to_owned()],
                        }],
                        is_final: true,
                    }],
                },
            ];
            Ok(Response::new(
                Box::pin(tokio_stream::iter(replies.into_iter().map(Ok))) as Responses,
            ))
        })
    }
}

struct Server {
    endpoint: String,
    fixture: RivaFixture,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(language: &'static str) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("grpc://{}", listener.local_addr().unwrap());
        let fixture = RivaFixture {
            language,
            fail: Arc::new(false.into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let service = fixture.clone();
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let incoming = tokio_stream::wrappers::TcpListenerStream::new(
                        tokio::net::TcpListener::from_std(listener).unwrap(),
                    );
                    let server = tonic::transport::Server::builder()
                        .add_service(service)
                        .serve_with_incoming_shutdown(incoming, async {
                            let _ = shutdown.await;
                        });
                    tokio::time::timeout(Duration::from_secs(30), server)
                        .await
                        .expect("bounded fixture lifetime")
                        .unwrap();
                });
        });
        Self {
            endpoint,
            fixture,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn transcribe(&self) -> Result<crate::dictate::TranscribeResult> {
        let audio = include_bytes!("../tests/fixtures/hello_speech.wav");
        let (bytes, rate) = decode_wav(audio)?;
        let pcm: Vec<f32> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0)
            .collect();
        let backend = crate::dictate::CloudTranscribeBackend::new_with_provider(
            crate::dictate::CloudTranscribeConfig {
                base_url: self.endpoint.clone(),
                api_key: String::new(),
                model: "nemotron-asr-streaming".to_owned(),
                timeout_ms: 5_000,
                language: None,
                prompt: None,
            },
            "nemotron",
        );
        backend
            .transcribe(&pcm, rate)
            .map_err(|error| anyhow!("{error:?}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn loopback_auto_mode_preserves_each_detected_language_on_the_wire() {
    // Fixture responses verify transport and language propagation independently;
    // they do not establish spoken-language recognition accuracy.
    for locale in ["en-US", "de-DE", "fr-FR", "da-DK"] {
        let server = Server::start(locale);
        let result = server.transcribe().unwrap();
        assert_eq!(result.text, "fixture final transcript");
        assert_eq!(result.language, locale);
        let seen = server.fixture.requests.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "auto");
        assert!(seen[0].1 > 0);
    }
}

#[test]
fn loopback_protocol_error_is_actionable_and_next_request_still_works() {
    let server = Server::start("da-DK");
    server
        .fixture
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = server.transcribe().unwrap_err().to_string();
    assert!(
        error.contains("Nemotron gRPC transcription failed"),
        "{error}"
    );
    assert!(error.contains("fixture rejected audio format"), "{error}");
    server
        .fixture
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(server.transcribe().unwrap().language, "da-DK");
    assert_eq!(server.fixture.requests.lock().unwrap().len(), 2);
}
