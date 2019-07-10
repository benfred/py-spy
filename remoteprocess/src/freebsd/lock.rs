use libc::{pid_t, waitpid, WIFSTOPPED};

use std::io::Error as IoError;

use super::Error;
use super::ptrace;

#[derive(Debug)]
pub struct ProcessLock {
    pid: pid_t,
}

impl ProcessLock {
    pub fn new(pid: pid_t) -> Result<Self, Error> {
        ptrace::attach(pid)?;
        let mut wait_status = 0;

        let stopped = unsafe {
            waitpid(pid, &mut wait_status as *mut _, 0);
            WIFSTOPPED(wait_status)
        };

        if !stopped {
            return Err(Error::IOError(IoError::last_os_error()));
        }

        Ok(ProcessLock { pid })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        ptrace::detach(self.pid);
    }
}
