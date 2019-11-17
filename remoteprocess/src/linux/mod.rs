#[cfg(all(feature="unwind", target_arch="x86_64"))]
pub mod libunwind;
#[cfg(all(feature="unwind", target_arch="x86_64"))]
mod symbolication;

use libc::pid_t;

use nix::{self, sys::wait, sys::ptrace, {sched::{setns, CloneFlags}}};
use std::collections::HashMap;
use std::convert::TryInto;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::fs::File;

use super::Error;

#[cfg(all(feature="unwind", target_arch="x86_64"))]
pub use self::symbolication::*;

#[cfg(all(feature="unwind", target_arch="x86_64"))]
pub use self::libunwind::Unwinder;

use read_process_memory::{CopyAddress, ProcessHandle};

pub type Pid = pid_t;
pub type Tid = pid_t;

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

    pub fn cmdline(&self) -> Result<Vec<String>, Error> {
        let mut f = std::fs::File::open(format!("/proc/{}/cmdline", self.pid))?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;

        let mut ret = Vec::new();
        for arg in buffer.split(|b| *b == 0) {
            ret.push(String::from_utf8(arg.to_vec())
                .map_err(|e| Error::Other(format!("Failed to convert ut8 {}", e)))?)
        }
        Ok(ret)
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

    pub fn child_processes(&self) -> Result<Vec<(Pid, Pid)>, Error> {
        let processes = get_process_tree()?;
        Ok(crate::filter_child_pids(self.pid, &processes))
    }

    #[cfg(all(feature="unwind", target_arch="x86_64"))]
    pub fn unwinder(&self) -> Result<Unwinder, Error> {
        Ok(Unwinder::new()?)
    }

    #[cfg(all(feature="unwind", target_arch="x86_64"))]
    pub fn symbolicator(&self) -> Result<Symbolicator, Error> {
        Ok(Symbolicator::new(self.pid)?)
    }
}

impl super::ProcessMemory for Process {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        let handle: ProcessHandle = self.pid.try_into()?;
        Ok(handle.copy_address(addr, buf)?)
    }
}

impl Thread {
    pub fn new(threadid: i32) -> Result<Thread, Error> {
        Ok(Thread{tid: nix::unistd::Pid::from_raw(threadid)})
    }

    pub fn lock(&self) -> Result<ThreadLock, Error> {
        Ok(ThreadLock::new(self.tid)?)
    }

    pub fn id(&self) -> Result<Tid, Error> {
        Ok(self.tid.as_raw())
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

fn get_process_tree() -> Result<HashMap<Pid, Pid>, Error> {
    let mut ret = HashMap::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let filename = entry.file_name();
        let pid = match filename.to_str() {
            Some(pid) => pid,
            None => continue
        };
        if let Ok(pid) = pid.parse::<Pid>() {
            match get_parent_pid(pid) {
                Ok(ppid) => { ret.insert(pid, ppid) },
                Err(_) => continue
            };
        }
    }
    Ok(ret)
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
            warn!("Failed to detach from thread {} : {}", self.tid, e);
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

    pub fn is_set(self) -> bool {
        self.ns_file.is_some()
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

fn get_parent_pid(pid: Pid) -> Result<Pid, Error> {
    let mut file = File::open(format!("/proc/{}/stat", pid))?;
    let mut buf=[0u8; 512];
    file.read(&mut buf)?;
    get_ppid_status(&buf).ok_or_else(|| Error::Other(format!("Failed to parse /proc/{}/stat", pid)))
}

fn get_ppid_status(stat: &[u8]) -> Option<Pid> {
    lazy_static! {
        static ref RE: regex::bytes::Regex = regex::bytes::Regex::new(r"^\d+ \(.+\) \w (\d+)").unwrap();
    }
    let caps = RE.captures(stat)?;
    std::str::from_utf8(caps.get(1)?.as_bytes()).ok()?.parse::<Pid>().ok()
}

#[test]
fn test_parse_active_stat() {
    assert_eq!(get_active_status(b"1234 (bash) S 1233"), Some(b'S'));
    assert_eq!(get_active_status(b"1234 (with space) R 1233"), Some(b'R'));
    assert_eq!(get_active_status(b"1234"), None);
    assert_eq!(get_active_status(b")"), None);
    assert_eq!(get_active_status(b")))"), None);
    assert_eq!(get_active_status(b"1234 (bash)S"), None);
    assert_eq!(get_active_status(b"1234)SSSS"), None);
    assert_eq!(get_active_status(b"15379 (ipython) t 9898 15379 9898 34816"), Some(b't'));
}


#[test]
fn test_parse_ppid_stat() {
    assert_eq!(get_ppid_status(b"1234 (bash) S 1233"), Some(1233));
    assert_eq!(get_ppid_status(b"1234 (with space) R 1233"), Some(1233));
    assert_eq!(get_ppid_status(b"1234"), None);
    assert_eq!(get_ppid_status(b")"), None);
    assert_eq!(get_ppid_status(b")))"), None);
    assert_eq!(get_ppid_status(b"1234 (bash)S"), None);
    assert_eq!(get_ppid_status(b"1234)SSSS"), None);
    assert_eq!(get_ppid_status(b"15379 (ipython) t 9898 15379 9898 34816"), Some(9898));
}
