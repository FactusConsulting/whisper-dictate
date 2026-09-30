//! Bound HTTP fixture reads without inheriting a listener's nonblocking mode.

use std::net::TcpStream;
use std::time::Duration;

pub(crate) fn configure(stream: &TcpStream) {
    // Windows can inherit the nonblocking listener's mode on accepted sockets.
    // Socket read/write timeouts do not make a nonblocking read wait for bytes.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
}

#[cfg(test)]
#[path = "test_http_stream_tests.rs"]
mod tests;
