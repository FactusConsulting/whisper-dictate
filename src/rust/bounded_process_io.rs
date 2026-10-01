use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Default)]
pub(super) struct Captured {
    pub(super) bytes: Vec<u8>,
    pub(super) truncated: bool,
    pub(super) error: Option<io::Error>,
}

pub(super) struct Worker {
    file: Option<Arc<File>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Captured>>,
}

impl Worker {
    pub(super) fn reader(file: File, stop: Arc<AtomicBool>, cap: usize) -> io::Result<Self> {
        Self::spawn(file, stop, move |file, stop| {
            let mut result = Captured::default();
            let mut buffer = [0; 8192];
            while !stop.load(Ordering::Acquire) {
                match (&*file).read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        result.bytes.extend_from_slice(&buffer[..count]);
                        if result.bytes.len() > cap {
                            result.truncated = true;
                            result.bytes.drain(..result.bytes.len() - cap);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        result.error = Some(error);
                        break;
                    }
                }
            }
            result
        })
    }

    pub(super) fn writer(file: File, stop: Arc<AtomicBool>, input: Vec<u8>) -> io::Result<Self> {
        Self::spawn(file, stop, move |file, stop| {
            let mut remaining = input.as_slice();
            let mut result = Captured::default();
            while !remaining.is_empty() && !stop.load(Ordering::Acquire) {
                match (&*file).write(remaining) {
                    Ok(0) => {
                        result.error = Some(io::ErrorKind::WriteZero.into());
                        break;
                    }
                    Ok(count) => remaining = &remaining[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => {
                        result.error = Some(error);
                        break;
                    }
                }
            }
            result
        })
    }

    fn spawn(
        file: File,
        stop: Arc<AtomicBool>,
        work: impl FnOnce(Arc<File>, Arc<AtomicBool>) -> Captured + Send + 'static,
    ) -> io::Result<Self> {
        let file = Arc::new(file);
        let thread_file = Arc::clone(&file);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("wd-helper-io".to_owned())
            .spawn(move || work(thread_file, thread_stop))?;
        Ok(Self {
            file: Some(file),
            stop,
            thread: Some(thread),
        })
    }

    pub(super) fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub(super) fn release_finished_pipe(&mut self) {
        if self.is_finished() {
            self.file.take();
        }
    }

    pub(super) fn finish(&mut self) -> io::Result<Captured> {
        if let Some(thread) = self.thread.as_ref() {
            while !thread.is_finished() {
                if self.stop.load(Ordering::Acquire) {
                    // Repeating closes the race between checking the stop
                    // flag and issuing a Windows synchronous/overlapped read.
                    if let Some(file) = &self.file {
                        super::platform::interrupt_pipe(file, thread);
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        self.file.take();
        self.thread
            .take()
            .map_or(Ok(Captured::default()), |thread| {
                thread
                    .join()
                    .map_err(|_| io::Error::other("helper I/O worker panicked"))
            })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_retains_the_capped_tail_and_joins_its_worker() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"abcdef").unwrap();
        std::io::Seek::rewind(&mut file).unwrap();
        let mut worker = Worker::reader(file, Arc::new(AtomicBool::new(false)), 3).unwrap();
        let captured = worker.finish().unwrap();
        assert_eq!(captured.bytes, b"def");
        assert!(captured.truncated);
        assert!(captured.error.is_none());
        assert!(worker.file.is_none());
        assert!(worker.thread.is_none());
    }
}
