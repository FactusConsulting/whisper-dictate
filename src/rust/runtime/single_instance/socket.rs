//! TCP-loopback transport + length-prefixed JSON framing for the
//! single-instance gate.
//!
//! Design: the running instance binds `127.0.0.1:0` (kernel-chosen
//! ephemeral port) and writes the resulting port into the lockfile. New
//! invocations read the lockfile, connect to the port, send their argv,
//! and exit — no signals, no D-Bus, no named pipes. Named pipes on
//! Windows and abstract sockets on Linux both work but require different
//! stdlib types + `unsafe` on Windows; TCP loopback is the one primitive
//! `std::net` gives us that works identically on every target we ship
//! for, without pulling a `interprocess` / `windows-sys` dep.
//!
//! Threat model: the socket is bound to the loopback interface so the
//! attack surface is other local user processes. To limit that surface
//! the client must present the per-process nonce from the lockfile as
//! the first field of its handshake message — a rogue process without
//! read access to the lockfile (which lives under `$XDG_RUNTIME_DIR`,
//! mode 0700 on Linux) can't spoof commands.
//!
//! Wire format:
//!
//! ```text
//! [4 bytes big-endian length N][N bytes JSON body]
//! ```
//!
//! JSON body:
//!
//! ```json
//! {"token": "<hex>", "argv": ["--toggle-recording"]}
//! ```
//!
//! The response is a single big-endian length-prefixed JSON envelope
//! (`{"ok": true}` or `{"ok": false, "error": "..."}`) so the client can
//! surface a useful error message when the server rejects the handshake.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Cap on the framed payload size. A CLI arg vector should be well under
/// this; the cap exists so a rogue local process can't wedge the server
/// by claiming a 4 GiB length prefix.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024;

/// How long to wait for the server to accept + reply. Tuned for the
/// "instance is genuinely running, take my args" case: local TCP + a
/// single JSON round-trip fits comfortably in a second, so anything
/// longer likely means the server thread wedged and we should fail loud
/// rather than block the user.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

/// One incoming forwarded command, as delivered to the owning instance
/// via the [`crate::runtime::single_instance::SingleInstance::try_recv`]
/// channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardedCommand {
    /// The argv the second instance was invoked with (without argv[0]).
    pub argv: Vec<String>,
}

/// Wire envelope sent by the client. Kept `pub(crate)` so tests can
/// construct it directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    pub token: String,
    pub argv: Vec<String>,
}

/// Wire envelope returned by the server. `ok=false` cases carry a short
/// human-readable reason so the client's exit message is actionable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Bind a fresh loopback listener and return the kernel-chosen port.
/// `port=0` asks the kernel for an ephemeral port; we read it back so
/// the lockfile records the actual value clients will connect to.
pub fn bind_loopback() -> io::Result<(TcpListener, u16)> {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    let listener = TcpListener::bind(addr)?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Serialise a length-prefixed frame to `stream`. The length prefix is
/// four big-endian bytes; the body follows. Any payload larger than
/// [`MAX_FRAME_BYTES`] is rejected up-front so we never advertise a
/// length the peer would refuse.
pub(crate) fn write_frame(stream: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len: u32 = body
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame exceeds MAX_FRAME_BYTES",
        ));
    }
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Read a length-prefixed frame from `stream`. Returns `Ok(None)` at
/// clean EOF (client closed without sending anything) so the server can
/// distinguish "no work" from a real error. Frames larger than
/// [`MAX_FRAME_BYTES`] are rejected to bound server memory use.
pub(crate) fn read_frame(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME_BYTES",
        ));
    }
    let mut body = vec![0u8; len as usize];
    stream.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Client side: connect to `127.0.0.1:port`, send a framed [`Request`],
/// read the framed [`Response`]. Returns `Ok(())` on `{"ok": true}`;
/// returns `Err` with the server's error string on `{"ok": false}` or
/// on any transport-level failure.
pub fn forward(port: u16, token: &str, argv: &[String]) -> io::Result<()> {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CLIENT_TIMEOUT)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;

    let req = Request {
        token: token.to_owned(),
        argv: argv.to_vec(),
    };
    let body =
        serde_json::to_vec(&req).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    write_frame(&mut stream, &body)?;

    let resp_bytes = read_frame(&mut stream)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "server closed early"))?;
    let resp: Response = serde_json::from_slice(&resp_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    if resp.ok {
        Ok(())
    } else {
        Err(io::Error::other(
            resp.error
                .unwrap_or_else(|| "server rejected request".to_owned()),
        ))
    }
}

/// Server side: read one framed [`Request`] from `stream`, check the
/// token, write a framed [`Response`]. On success returns the argv the
/// listener should hand up to the owning instance. On any failure
/// returns `Err` with a short diagnostic; the caller logs it and drops
/// the connection.
pub(crate) fn serve_one(
    stream: &mut TcpStream,
    expected_token: &str,
) -> io::Result<ForwardedCommand> {
    // Bound reads so a malicious client can't hold the accept thread
    // hostage.
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;

    let raw = match read_frame(stream)? {
        Some(bytes) => bytes,
        None => {
            let _ = write_response(
                stream,
                &Response {
                    ok: false,
                    error: Some("empty request".to_owned()),
                },
            );
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "empty request",
            ));
        }
    };

    let req: Request = match serde_json::from_slice(&raw) {
        Ok(req) => req,
        Err(e) => {
            let _ = write_response(
                stream,
                &Response {
                    ok: false,
                    error: Some(format!("malformed request: {e}")),
                },
            );
            return Err(io::Error::new(io::ErrorKind::InvalidData, e));
        }
    };

    if req.token != expected_token {
        let _ = write_response(
            stream,
            &Response {
                ok: false,
                error: Some("token mismatch".to_owned()),
            },
        );
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "token mismatch",
        ));
    }

    write_response(
        stream,
        &Response {
            ok: true,
            error: None,
        },
    )?;
    Ok(ForwardedCommand { argv: req.argv })
}

fn write_response(stream: &mut TcpStream, resp: &Response) -> io::Result<()> {
    let body =
        serde_json::to_vec(resp).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    write_frame(stream, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_frame_encode_decode() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, b"hello world").unwrap();
        let mut cursor = Cursor::new(buf);
        let out = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn read_frame_returns_none_at_clean_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn read_frame_rejects_truncated_body() {
        // 4-byte prefix says "10 bytes" but only 3 follow.
        let mut cursor = Cursor::new(vec![0, 0, 0, 10, 1, 2, 3]);
        assert!(read_frame(&mut cursor).is_err());
    }

    #[test]
    fn read_frame_rejects_oversized_prefix() {
        let mut prefix = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();
        prefix.extend(vec![0u8; 4]);
        let mut cursor = Cursor::new(prefix);
        let err = read_frame(&mut cursor).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn write_frame_rejects_oversized_body() {
        let mut buf: Vec<u8> = Vec::new();
        let big = vec![0u8; (MAX_FRAME_BYTES + 1) as usize];
        let err = write_frame(&mut buf, &big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn request_round_trip_serde() {
        let req = Request {
            token: "abc123".to_owned(),
            argv: vec!["--toggle-recording".to_owned(), "--config".to_owned()],
        };
        let raw = serde_json::to_vec(&req).unwrap();
        let decoded: Request = serde_json::from_slice(&raw).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn response_ok_omits_error_field_on_wire() {
        let raw = serde_json::to_string(&Response {
            ok: true,
            error: None,
        })
        .unwrap();
        assert!(!raw.contains("error"));
    }

    #[test]
    fn response_error_survives_round_trip() {
        let resp = Response {
            ok: false,
            error: Some("token mismatch".to_owned()),
        };
        let raw = serde_json::to_string(&resp).unwrap();
        let decoded: Response = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn bind_loopback_returns_usable_port() {
        let (listener, port) = bind_loopback().unwrap();
        assert!(port > 0);
        // Sanity-check the bound address is loopback so we don't
        // accidentally advertise 0.0.0.0 in the lockfile.
        let local = listener.local_addr().unwrap();
        assert!(local.ip().is_loopback());
    }

    /// End-to-end: server accepts one connection, verifies the token,
    /// hands the argv up; client sees `ok=true`.
    #[test]
    fn forward_then_serve_round_trip() {
        let (listener, port) = bind_loopback().unwrap();
        let expected_token = "correct-horse-battery-staple".to_owned();
        let expected_argv = vec![
            "--toggle-recording".to_owned(),
            "--config".to_owned(),
            "/tmp/x.yaml".to_owned(),
        ];

        let server_token = expected_token.clone();
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().unwrap();
            serve_one(&mut stream, &server_token).unwrap()
        });

        forward(port, &expected_token, &expected_argv).unwrap();
        let received = server_thread.join().unwrap();
        assert_eq!(received.argv, expected_argv);
    }

    /// Client presents a bad token; server rejects with a diagnostic and
    /// the client sees an `Err` carrying the server's error message.
    #[test]
    fn forward_with_wrong_token_is_rejected() {
        let (listener, port) = bind_loopback().unwrap();

        let server_thread = std::thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().unwrap();
            // Deliberately ignore the returned Result so the test doesn't
            // panic when the wrong-token path returns Err — that's the
            // path we're exercising here.
            let _ = serve_one(&mut stream, "correct-token");
        });

        let err = forward(port, "wrong-token", &[]).unwrap_err();
        server_thread.join().unwrap();
        // The server's error string flows through `Response.error` and
        // into the client's `io::Error`, so we can assert on it.
        assert!(
            err.to_string().contains("token mismatch"),
            "expected token-mismatch diagnostic, got: {err}"
        );
    }

    /// Client connects to a port with no server behind it; the connect
    /// call surfaces the OS refusal quickly rather than hanging until the
    /// full timeout.
    #[test]
    fn forward_to_unbound_port_fails_fast() {
        // Bind a fresh listener just to grab a known-free port, then drop
        // it so the port is free again for the connect attempt.
        let port = {
            let (listener, port) = bind_loopback().unwrap();
            drop(listener);
            port
        };
        let err = forward(port, "any", &[]).unwrap_err();
        // We accept either ConnectionRefused (Linux) or TimedOut (some
        // Windows configurations quietly drop packets instead of RSTing)
        // — both are "no server" signals.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
            ),
            "unexpected error: {err:?}"
        );
    }
}
