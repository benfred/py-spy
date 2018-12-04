use std;
use failure::Error;
use read_process_memory::{Pid, ProcessHandle};

#[cfg(target_os = "macos")]
mod os_impl {
    use super::*;
    use libproc::libproc::proc_pid::pidpath;
    use mach;
    use mach::kern_return::{KERN_SUCCESS};
    use mach::port::{mach_port_name_t};

    pub fn get_exe(pid: Pid) -> Result<String, Error> {
        pidpath(pid).map_err(|e| format_err!("proc_pidpath failed: {}", e))
    }

    pub struct Lock {
        task: mach_port_name_t
    }

    impl Lock {
        pub fn new(task: &ProcessHandle) -> Result<Lock, Error> {
            let result = unsafe { mach::task::task_suspend(*task) };
            if result != KERN_SUCCESS {
                return Err(Error::from(std::io::Error::last_os_error()));
            }
            Ok(Lock{task: task.clone()})
        }
    }
    impl Drop for Lock {
        fn drop (&mut self) {
            let result = unsafe { mach::task::task_resume(self.task) };
            if result != KERN_SUCCESS {
                error!("Failed to resume task {}: {}", self.task, std::io::Error::last_os_error());
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod os_impl {
    use super::*;
    use nix::{self, {sys::{ptrace, wait}, {unistd::Pid as Tid}}};

    /// This locks a target process using ptrace, and prevents it from running while this
    /// struct is alive
    pub struct Lock {
        #[allow(dead_code)]
        locks: Vec<ThreadLock>
    }

    impl Lock {
        pub fn new(process: &ProcessHandle) -> Result<Lock, Error> {
            let mut locks = Vec::new();
            let mut locked = std::collections::HashSet::new();
            let mut done = false;

            // we need to lock each invidual thread of the process, but
            // while we're doing this new threads could be created. keep
            // on creating new locks for each thread until no new locks are
            // created
            while !done {
                done = true;
                for threadid in threads(process)? {
                    if !locked.contains(&threadid) {
                        locks.push(ThreadLock::new(Tid::from_raw(threadid))?);
                        locked.insert(threadid);
                        done = false;
                    }
                }
            }

            Ok(Lock{locks})
        }
    }

    pub fn get_exe(pid: Pid) -> Result<String, Error> {
        let path = std::fs::read_link(format!("/proc/{}/exe", pid))?;
        Ok(path.to_string_lossy().to_string())
    }

    struct ThreadLock {
        tid: Tid
    }

    impl ThreadLock {
        fn new(tid: Tid) -> Result<ThreadLock, nix::Error> {
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

    fn threads(process: &ProcessHandle) -> Result<Vec<i32>, std::io::Error> {
        let mut ret = Vec::new();
        let path = format!("/proc/{}/task", process);
        let tasks = std::fs::read_dir(path)?;
        for entry in tasks {
            let entry = entry?;
            let filename = entry.file_name();
            let thread = match filename.to_str() {
                Some(thread) => thread,
                None => continue
            };

            if let Ok(threadid) = thread.parse::<i32>() {
                ret.push(threadid);
            }
        }
        Ok(ret)
    }
}

#[cfg(windows)]
mod os_impl {
    use super::*;
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winnt::{PROCESS_QUERY_INFORMATION, WCHAR, HANDLE, PROCESS_VM_READ, PROCESS_SUSPEND_RESUME};
    use winapi::shared::minwindef::{FALSE, DWORD, MAX_PATH, ULONG};
    use winapi::um::handleapi::{INVALID_HANDLE_VALUE, CloseHandle};
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStringExt};
    use winapi::shared::ntdef::NTSTATUS;

    pub fn open(pid: Pid) -> Result<ProcessHandle, Error> {
        // we can't just use try_into_prcess_handle here because we need some additional permissions
        unsafe {
            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_SUSPEND_RESUME, FALSE, pid);
            if handle == (0 as std::os::windows::io::RawHandle) {
                return Err(Error::from(std::io::Error::last_os_error()));
            }
            Ok(handle)
        }
    }

    // using these undocumented api's seems to be the best way to suspend/resume a process
    // on windows (using the toolhelp32snapshot api to get threads doesn't seem practical tbh)
    #[link(name="ntdll")]
    extern "system" {
        fn RtlNtStatusToDosError(status: NTSTATUS) -> ULONG;
        fn NtSuspendProcess(process: HANDLE) -> NTSTATUS;
        fn NtResumeProcess(process: HANDLE) -> NTSTATUS;
    }

    pub struct Lock {
        process: HANDLE
    }

    impl Lock {
        pub fn new(process: &ProcessHandle) -> Result<Lock, Error> {
            let process = *process;
            unsafe {
                let ret = NtSuspendProcess(process);
                if ret != 0 {
                    return Err(Error::from(std::io::Error::from_raw_os_error(RtlNtStatusToDosError(ret) as i32)));
                }
            }
            Ok(Lock{process})
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            unsafe {
                let ret = NtResumeProcess(self.process);
                if ret != 0 {
                    panic!("Failed to resume process: {}",
                           std::io::Error::from_raw_os_error(RtlNtStatusToDosError(ret) as i32));
                }
            }
        }
    }

    pub fn get_exe(pid: Pid) -> Result<String, Error> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, pid as DWORD);
            if process == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().into());
            }

            let mut size = MAX_PATH as DWORD;
            let mut filename: [WCHAR; MAX_PATH] = std::mem::zeroed();
            let ret = QueryFullProcessImageNameW(process, 0, filename.as_mut_ptr(), &mut size);
            CloseHandle(process);

            if ret == 0 {
                return Err(std::io::Error::last_os_error().into());
            }

            Ok(OsString::from_wide(&filename[0..size as usize]).to_string_lossy().into_owned())
        }
    }
}
pub use self::os_impl::*;

#[cfg(unix)]
pub fn open(pid: Pid) -> Result<ProcessHandle, Error> {
    use read_process_memory::TryIntoProcessHandle;
    Ok(pid.try_into_process_handle()?)
}
