pub mod libunwind;
mod gimli_unwinder;
mod symbolication;

use goblin::error::Error as GoblinError;

use nix::{self, sys::wait, {sched::{setns, CloneFlags}}};
use std::os::unix::io::AsRawFd;
use std::fs::File;

use super::Error;
pub use self::gimli_unwinder::*;
pub use self::symbolication::*;
pub use self::libunwind::{LibUnwind};

pub use read_process_memory::{Pid, ProcessHandle};
pub use nix::unistd::Pid as Tid;

pub struct Process {
    pub pid: Pid,
}

impl Process {
    pub fn new(pid: Pid) -> Result<Process, Error> {
        Ok(Process{pid})
    }

    pub fn handle(&self) -> ProcessHandle { self.pid }

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
            for threadid in self.threads()? {
                if !locked.contains(&threadid) {
                    locks.push(ThreadLock::new(threadid).map_err(|e| Error::Other(format!("Failed to lock {:?}", e)))?);
                    locked.insert(threadid);
                    done = false;
                }
            }
        }

        Ok(Lock{locks})
    }

    pub fn threads(&self) -> Result<Vec<Tid>, Error> {
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
                ret.push(Tid::from_raw(threadid));
            }
        }
        Ok(ret)
    }

    pub fn unwinder(&self) -> Result<Unwinder, GoblinError> {
        Unwinder::new(self.pid)
    }
}


/// This locks a target process using ptrace, and prevents it from running while this
/// struct is alive
pub struct Lock {
    #[allow(dead_code)]
    locks: Vec<ThreadLock>
}

struct ThreadLock {
    tid: Tid
}

impl ThreadLock {
    fn new(tid: Tid) -> Result<ThreadLock, nix::Error> {
        nix::sys::ptrace::attach(tid)?;
        wait::waitpid(tid, Some(wait::WaitPidFlag::WSTOPPED))?;
        debug!("attached to thread {}", tid);
        Ok(ThreadLock{tid})
    }
}

impl Drop for ThreadLock {
    fn drop(&mut self) {
        if let Err(e) = nix::sys::ptrace::detach(self.tid) {
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