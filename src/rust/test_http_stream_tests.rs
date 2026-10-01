use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

#[test]
fn accepted_nonblocking_socket_waits_for_delayed_request_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    // Deterministically model Windows accepting from a nonblocking listener.
    stream.set_nonblocking(true).unwrap();
    super::configure(&stream);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            client.write_all(b"request").unwrap();
        });
        let mut buffer = [0; 7];
        stream.read_exact(&mut buffer).unwrap();
        assert_eq!(&buffer, b"request");
    });
    assert_eq!(stream.read_timeout().unwrap(), Some(Duration::from_secs(2)));
}
