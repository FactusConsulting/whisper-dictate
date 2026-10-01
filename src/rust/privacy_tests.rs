use super::*;

#[test]
fn local_only_rejects_remote_authorities_with_loopback_query_or_fragment() {
    for url in [
        "https://remote.example?user=person@localhost",
        "https://remote.example#person@127.0.0.1",
        "https://remote.example?user=person@[::1]",
        "https://localhost@remote.example/v1",
        "https://127.0.0.1@remote.example/v1",
        "https://remote.example/v1/localhost",
        "https://localhost.remote.example/v1",
    ] {
        assert!(!is_loopback_url(url), "remote endpoint allowed: {url}");
        assert!(assert_local_backend(true, "openai", "STT", Some(url)).is_err());
    }
}

#[test]
fn local_only_retains_valid_loopback_endpoints() {
    for url in [
        "http://LOCALHOST:8000/v1?user=remote.example",
        "http://localhost./v1",
        "http://127.0.0.1:8000/v1#remote.example",
        "https://127.0.0.5/v1",
        "http://[::1]:8000/v1",
        "http://user:pass@localhost:8000/v1",
        "grpc://localhost:50051",
        "localhost:50051",
    ] {
        assert!(is_loopback_url(url), "loopback endpoint rejected: {url}");
        assert_local_backend(true, "openai", "STT", Some(url)).unwrap();
    }
}

#[test]
fn malformed_or_unsupported_loopback_urls_fail_closed() {
    for url in [
        "",
        "http://",
        "http://[::1",
        "http://localhost:invalid/v1",
        "http://localhost:/v1",
        "http://localhost:999999/v1",
        "http://[::1]:invalid/v1",
        "http://local host/v1",
        "http://localhost\\@remote.example/v1",
        "ftp://localhost/v1",
        "/localhost/v1",
    ] {
        assert!(!is_loopback_url(url), "malformed endpoint allowed: {url}");
    }
}
