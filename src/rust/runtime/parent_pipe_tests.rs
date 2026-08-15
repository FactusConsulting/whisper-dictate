use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::parent_pipe::spawn_eof_watchdog;

#[test]
fn parent_pipe_eof_invokes_the_shutdown_action() {
    let invoked = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&invoked);
    let watcher = spawn_eof_watchdog(std::io::Cursor::new(Vec::<u8>::new()), move || {
        observed.store(true, Ordering::SeqCst);
    })
    .expect("spawn parent-pipe watchdog");

    watcher.join().expect("join parent-pipe watchdog");
    assert!(invoked.load(Ordering::SeqCst));
}
