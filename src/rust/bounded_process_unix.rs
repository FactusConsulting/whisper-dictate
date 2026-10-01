use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::thread::JoinHandle;

pub(super) struct ProcessTree {
    group: Option<i32>,
}

pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessTree)> {
    command.process_group(0);
    let child = command.spawn()?;
    let group = child.id() as i32;
    Ok((child, ProcessTree { group: Some(group) }))
}

impl ProcessTree {
    pub(super) fn wait_terminated(&self) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn disarm(&mut self) -> io::Result<()> {
        self.group = None;
        Ok(())
    }
    pub(super) fn terminate(&mut self) {
        if let Some(group) = self.group.take() {
            // SAFETY: this is the private process group created for our child.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }
}

pub(super) fn pipe_file(pipe: impl IntoRawFd) -> io::Result<File> {
    // SAFETY: ownership transfers from the child pipe to this File.
    let file = unsafe { File::from_raw_fd(pipe.into_raw_fd()) };
    // Nonblocking I/O lets a cancelled worker exit even when a descendant
    // deliberately left its process group and holds an inherited pipe open.
    let fd = file.as_raw_fd();
    // SAFETY: fd is owned by file and both fcntl commands take an integer flag.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

pub(super) fn interrupt_pipe<T>(_file: &File, _thread: &JoinHandle<T>) {}
