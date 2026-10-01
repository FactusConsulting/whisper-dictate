use super::{probe_backend, read_request_from};
use crate::injection::dispatcher::InjectRequest;

#[test]
fn stdin_reader_decodes_the_complete_request_without_initializing_a_backend() {
    let request = read_request_from(&b"{\"action\":\"inject\",\"text\":\"fixture\"}"[..]).unwrap();
    assert!(matches!(request, InjectRequest::Inject { text, .. } if text == "fixture"));
    assert!(matches!(
        read_request_from(&b"{\"action\":\"probe\"}"[..]).unwrap(),
        InjectRequest::Probe
    ));
}

#[test]
fn stdin_reader_reports_malformed_and_failed_input() {
    assert!(read_request_from(&b"not json"[..]).is_err());
    struct FailedRead;
    impl std::io::Read for FailedRead {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic input failure"))
        }
    }
    assert!(read_request_from(FailedRead)
        .unwrap_err()
        .to_string()
        .contains("synthetic input failure"));
}

#[test]
fn probe_reports_the_actual_platform_and_build_feature_without_injection() {
    let response = probe_backend();
    assert_eq!(response.platform, std::env::consts::OS);
    assert_eq!(response.feature_enabled, cfg!(feature = "rust-injection"));
    #[cfg(not(target_os = "linux"))]
    {
        assert!(response.linux_session.is_none());
        assert!(response.linux_helper.is_none());
    }
}
