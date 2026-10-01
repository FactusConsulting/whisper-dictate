use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenThread, ResumeThread, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, THREAD_SUSPEND_RESUME,
};
use windows_sys::Win32::System::IO::{CancelIoEx, CancelSynchronousIo};

pub(super) struct ProcessTree {
    job: Handle,
    terminated: bool,
    terminating_processes: Vec<Handle>,
    termination_error: Option<io::Error>,
}

pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, ProcessTree)> {
    // SAFETY: no attributes/name; the owned job handle is not inheritable.
    let job = Handle::new(unsafe { CreateJobObjectW(null(), null()) })?;
    // SAFETY: zero initializes all optional limits and accounting fields.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the structure and byte size match the requested info class.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // Stable Rust exposes no job-list attribute at spawn. Suspend until the
    // child belongs to our job, so it cannot fork outside it in the meantime.
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = command.spawn()?;
    // SAFETY: both live handles have the rights returned by CreateProcess/job creation.
    let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle()) };
    let setup = if assigned == 0 {
        Err(io::Error::last_os_error())
    } else {
        resume_primary_thread(child.id())
    };
    if let Err(error) = setup {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((
        child,
        ProcessTree {
            job,
            terminated: false,
            terminating_processes: Vec::new(),
            termination_error: None,
        },
    ))
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    // SAFETY: snapshot includes thread entries; no borrowed buffers.
    let snapshot = Handle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    // The suspended process has not executed code or created child threads.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    // SAFETY: entry is correctly sized and the snapshot stays alive.
    let mut present = unsafe { Thread32First(snapshot.0, &mut entry) };
    while present != 0 {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: the thread id came from this process's snapshot entry.
            let thread =
                Handle::new(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
            // SAFETY: this handle owns THREAD_SUSPEND_RESUME access.
            if unsafe { ResumeThread(thread.0) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: same live snapshot and sized output buffer as above.
        present = unsafe { Thread32Next(snapshot.0, &mut entry) };
    }
    Err(io::Error::other(
        "suspended helper's primary thread was not found",
    ))
}

impl ProcessTree {
    pub(super) fn wait_terminated(&self) -> io::Result<()> {
        // Termination is asynchronous on Windows; closing the pipes alone
        // does not prove the descendants have finished exiting.
        let started = std::time::Instant::now();
        if let Some(error) = &self.termination_error {
            return Err(io::Error::new(error.kind(), error.to_string()));
        }
        loop {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION =
                unsafe { std::mem::zeroed() };
            // SAFETY: live owned job, correctly sized accounting buffer.
            if unsafe {
                QueryInformationJobObject(
                    self.job.0,
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut _,
                    std::mem::size_of_val(&accounting) as u32,
                    null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Retained handles cannot refer to a reused PID. Job accounting
            // alone is not the process object's termination/signal barrier.
            let signaled = self.terminating_processes.iter().all(|process| {
                // SAFETY: each uniquely owned process handle has SYNCHRONIZE.
                unsafe { WaitForSingleObject(process.0, 0) == WAIT_OBJECT_0 }
            });
            if accounting.ActiveProcesses == 0 && signaled {
                return Ok(());
            }
            if started.elapsed() >= std::time::Duration::from_secs(1) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "helper descendants did not finish terminating",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub(super) fn disarm(&mut self) -> io::Result<()> {
        let limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: same owned job and matching structure as at creation.
        if unsafe {
            SetInformationJobObject(
                self.job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        self.terminated = true;
        Ok(())
    }
    pub(super) fn terminate(&mut self) {
        if !self.terminated {
            match process_handles(self.job.0) {
                Ok(processes) => self.terminating_processes = processes,
                Err(error) => self.termination_error = Some(error),
            }
            self.terminated = true;
            // SAFETY: this job contains only the helper and its descendants.
            if unsafe { TerminateJobObject(self.job.0, 1) } == 0 {
                self.termination_error = Some(io::Error::last_os_error());
            }
        }
    }
}

fn process_handles(job: HANDLE) -> io::Result<Vec<Handle>> {
    // Matching variable-length JOBOBJECT_BASIC_PROCESS_ID_LIST layout. A
    // helper with more than 256 live processes is still killed, but cleanup
    // is reported as unverified rather than allocating an unbounded census.
    #[repr(C)]
    struct ProcessIds {
        assigned: u32,
        returned: u32,
        pids: [usize; 256],
    }
    let mut ids = ProcessIds {
        assigned: 0,
        returned: 0,
        pids: [0; 256],
    };
    // SAFETY: correctly aligned/sized list buffer and live job query handle.
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            &mut ids as *mut _ as *mut _,
            std::mem::size_of_val(&ids) as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if ids.assigned > ids.returned || ids.returned as usize > ids.pids.len() {
        return Err(io::Error::other(
            "helper job process census exceeded its bounded capacity",
        ));
    }
    let mut processes = Vec::new();
    for pid in &ids.pids[..ids.returned as usize] {
        // SAFETY: query rights verify membership; SYNCHRONIZE permits waiting.
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                *pid as u32,
            )
        };
        if process.is_null() {
            // A process may exit between the census and opening its handle.
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                continue;
            }
            return Err(error);
        }
        let process = Handle::new(process)?;
        let mut belongs = 0;
        // SAFETY: both handles are live with the required query rights.
        if unsafe { IsProcessInJob(process.0, job, &mut belongs) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if belongs != 0 {
            processes.push(process);
        }
    }
    Ok(processes)
}

pub(super) fn pipe_file(pipe: impl IntoRawHandle) -> io::Result<File> {
    // SAFETY: ownership transfers from the child pipe to File.
    Ok(unsafe { File::from_raw_handle(pipe.into_raw_handle()) })
}

pub(super) fn interrupt_pipe<T>(file: &File, thread: &JoinHandle<T>) {
    // SAFETY: Arc<File> and JoinHandle keep both handles alive through join.
    // Cancel both modes because std may use synchronous or overlapped pipes.
    unsafe {
        CancelIoEx(file.as_raw_handle(), null_mut());
        CancelSynchronousIo(thread.as_raw_handle());
    }
}

struct Handle(HANDLE);
impl Handle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this is our uniquely owned live kernel handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_handles_are_never_owned_or_closed() {
        assert!(Handle::new(null_mut()).is_err());
        assert!(Handle::new(INVALID_HANDLE_VALUE).is_err());
    }
}
