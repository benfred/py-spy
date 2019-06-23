#[cfg(unwind)]
pub mod libunwind;
#[cfg(unwind)]
mod gimli_unwinder;
#[cfg(unwind)]
mod symbolication;
use libc::{c_void, pid_t};

use nix::{self, sys::wait, sys::ptrace, {sched::{setns, CloneFlags}}};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::fs::File;

#[cfg(unwind)]
use crate::dwarf_unwind::Registers;
use super::Error;

#[cfg(unwind)]
pub use self::gimli_unwinder::*;
#[cfg(unwind)]
pub use self::symbolication::*;
#[cfg(unwind)]
pub use self::libunwind::{LibUnwind};

use read_process_memory::{CopyAddress};

pub type Pid = pid_t;

pub struct Process {
    pub pid: Pid,
}

#[derive(Eq, PartialEq, Hash, Copy, Clone)]
pub struct Thread {
    tid: nix::unistd::Pid
}

impl Process {
    pub fn new(pid: Pid) -> Result<Process, Error> {
        Ok(Process{pid})
    }

    pub fn exe(&self) -> Result<String, Error> {
        let path = std::fs::read_link(format!("/proc/{}/exe", self.pid))?;
        Ok(path.to_string_lossy().to_string())
    }

    pub fn cwd(&self) -> Result<String, Error> {
        let path = std::fs::read_link(format!("/proc/{}/cwd", self.pid))?;
        Ok(path.to_string_lossy().to_string())
    }

    pub fn lock(&self) -> Result<Lock, Error> {
        let mut locks = Vec::new();
        let mut locked = std::collections::HashSet::new();
        let mut done = false;

        // we need to lock each invidual thread of the process, but
        // while we're doing this new threads could be created. keep
        // on creating new locks for each thread until no new locks are
        // created
        while !done {
            done = true;
            for thread in self.threads()? {
                let threadid = thread.id()?;
                if !locked.contains(&threadid) {
                    locks.push(thread.lock()?);
                    locked.insert(threadid);
                    done = false;
                }
            }
        }

        Ok(Lock{locks})
    }

    pub fn threads(&self) -> Result<Vec<Thread>, Error> {
        let mut ret = Vec::new();
        let path = format!("/proc/{}/task", self.pid);
        let tasks = std::fs::read_dir(path)?;
        for entry in tasks {
            let entry = entry?;
            let filename = entry.file_name();
            let thread = match filename.to_str() {
                Some(thread) => thread,
                None => continue
            };

            if let Ok(threadid) = thread.parse::<i32>() {
                ret.push(Thread{tid: nix::unistd::Pid::from_raw(threadid)});
            }
        }
        Ok(ret)
    }

    #[cfg(unwind)]
    pub fn unwinder(&self) -> Result<Unwinder, Error> {
        Unwinder::new(self.pid)
    }
}

impl super::ProcessMemory for Process {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        Ok(self.pid.copy_address(addr, buf)?)
    }
}

impl Thread {
    pub fn new(threadid: i32) -> Thread{
        Thread{tid: nix::unistd::Pid::from_raw(threadid)}
    }

    pub fn lock(&self) -> Result<ThreadLock, Error> {
        Ok(ThreadLock::new(self.tid)?)
    }

    #[cfg(unwind)]
    pub fn registers(&self) -> Result<Registers, Error> {
        unsafe {
            let mut data: Registers = std::mem::zeroed();
            // nix has marked this as deprecated (in favour of specific functions like attach)
            // but hasn't yet exposed PTRACE_GETREGS as it's own function
            #[allow(deprecated)]
            ptrace::ptrace(ptrace::Request::PTRACE_GETREGS, self.tid,
                            std::ptr::null_mut(),
                            &mut data as *mut _ as * mut c_void)?;
            Ok(data)
        }
    }

    pub fn id(&self) -> Result<u64, Error> {
        Ok(self.tid.as_raw() as u64)
    }

    pub fn active(&self) -> Result<bool, Error> {
        let mut file = File::open(format!("/proc/{}/stat", self.tid))?;

        let mut buf=[0u8; 512];
        file.read(&mut buf)?;
        match get_active_status(&buf) {
            Some(stat) => Ok(stat == b'R'),
            None => Err(Error::Other(format!("Failed to parse /proc/{}/stat", self.tid)))
        }
    }
}

/// This locks a target process using ptrace, and prevents it from running while this
/// struct is alive
pub struct Lock {
    #[allow(dead_code)]
    locks: Vec<ThreadLock>
}

pub struct ThreadLock {
    tid: nix::unistd::Pid
}

impl ThreadLock {
    fn new(tid: nix::unistd::Pid) -> Result<ThreadLock, nix::Error> {
        ptrace::attach(tid)?;
        wait::waitpid(tid, Some(wait::WaitPidFlag::WSTOPPED))?;
        debug!("attached to thread {}", tid);
        Ok(ThreadLock{tid})
    }
}

impl Drop for ThreadLock {
    fn drop(&mut self) {
        if let Err(e) = ptrace::detach(self.tid) {
            error!("Failed to detach from thread {} : {}", self.tid, e);
        }
        debug!("detached from thread {}", self.tid);
    }
}

pub struct Namespace {
    ns_file: Option<File>
}

impl Namespace {
    pub fn new(pid: Pid) -> Result<Namespace, Error> {
        let target_ns_filename = format!("/proc/{}/ns/mnt", pid);
        let self_mnt = std::fs::read_link("/proc/self/ns/mnt")?;
        let target_mnt = std::fs::read_link(&target_ns_filename)?;
        if self_mnt != target_mnt {
            info!("Process {} appears to be running in a different namespace - setting namespace to match", pid);
            let target = File::open(target_ns_filename)?;
            // need to open this here, gets trickier after changing the namespace
            let self_ns = File::open("/proc/self/ns/mnt")?;
            setns(target.as_raw_fd(), CloneFlags::from_bits_truncate(0))?;
            Ok(Namespace{ns_file: Some(self_ns)})
        } else {
            info!("Target process is running in same namespace - not changing");
            Ok(Namespace{ns_file: None})
        }
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if let Some(ns_file) = self.ns_file.as_ref() {
            setns(ns_file.as_raw_fd(), CloneFlags::from_bits_truncate(0)).unwrap();
            info!("Restored process namespace");
        }
    }
}

fn get_active_status(stat: &[u8]) -> Option<u8> {
    // find the first ')' character, and return the active status
    // field which comes after it
    let mut iter = stat.iter().skip_while(|x| **x != b')');
    match (iter.next(), iter.next(), iter.next()) {
        (Some(b')'), Some(b' '), ret) => ret.map(|x| *x),
        _ => None
    }
}

#[test]
fn test_parse_stat() {
    assert_eq!(get_active_status(b"1234 (bash) S 1233"), Some(b'S'));
    assert_eq!(get_active_status(b"1234 (with space) R 1233"), Some(b'R'));
    assert_eq!(get_active_status(b"1234"), None);
    assert_eq!(get_active_status(b")"), None);
    assert_eq!(get_active_status(b")))"), None);
    assert_eq!(get_active_status(b"1234 (bash)S"), None);
    assert_eq!(get_active_status(b"1234)SSSS"), None);
}
