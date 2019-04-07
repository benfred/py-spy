use winapi::um::processthreadsapi::{OpenProcess, GetThreadId, SuspendThread, ResumeThread};
use winapi::um::winnt::{ACCESS_MASK, MAXIMUM_ALLOWED, PROCESS_QUERY_INFORMATION,
                        PROCESS_VM_READ, PROCESS_SUSPEND_RESUME, THREAD_QUERY_INFORMATION, THREAD_GET_CONTEXT,
                        WCHAR, HANDLE};
use winapi::shared::minwindef::{FALSE, DWORD, MAX_PATH, ULONG};
use winapi::um::handleapi::{CloseHandle};
use winapi::um::winbase::QueryFullProcessImageNameW;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStringExt};
use winapi::shared::ntdef::{PVOID, NTSTATUS, USHORT, VOID, NULL};

pub use read_process_memory::{Pid, ProcessHandle, CopyAddress};

use super::Error;

mod unwinder;
pub use self::unwinder::Unwinder;

pub struct Process {
    pub pid: Pid,
    pub handle: ProcessHandle
}

#[link(name="ntdll")]
extern "system" {
    // using these undocumented api's seems to be the best way to suspend/resume a process
    // on windows (using the toolhelp32snapshot api to get threads doesn't seem practical tbh)
    // https://j00ru.vexillium.org/2009/08/suspending-processes-in-windows/
    fn RtlNtStatusToDosError(status: NTSTATUS) -> ULONG;
    fn NtSuspendProcess(process: HANDLE) -> NTSTATUS;
    fn NtResumeProcess(process: HANDLE) -> NTSTATUS;

    fn NtQueryInformationThread(thread: HANDLE, info_class: u32, info: PVOID, info_len: ULONG, ret_len: * mut ULONG) -> NTSTATUS;

    // Use NtGetNextThread to get process threads. This limits us to Windows Vista and above,
    fn NtGetNextThread(process: HANDLE, thread: HANDLE, access: ACCESS_MASK, attritubes: ULONG, flags: ULONG, new_thread: *mut HANDLE) -> NTSTATUS;
}

impl Process {
    pub fn new(pid: Pid) -> Result<Process, Error> {
        // we can't just use try_into_prcess_handle here because we need some additional permissions
        unsafe {
            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_SUSPEND_RESUME | PROCESS_QUERY_INFORMATION
                                     | THREAD_QUERY_INFORMATION | THREAD_GET_CONTEXT, FALSE, pid);
            if handle == (0 as std::os::windows::io::RawHandle) {
                return Err(Error::from(std::io::Error::last_os_error()));
            }
            Ok(Process{pid, handle})
        }
    }

    pub fn handle(&self) -> ProcessHandle { self.handle }

    pub fn exe(&self) -> Result<String, Error> {
        unsafe {
            let mut size = MAX_PATH as DWORD;
            let mut filename: [WCHAR; MAX_PATH] = std::mem::zeroed();
            let ret = QueryFullProcessImageNameW(self.handle, 0, filename.as_mut_ptr(), &mut size);
            if ret == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(OsString::from_wide(&filename[0..size as usize]).to_string_lossy().into_owned())
        }
    }

    pub fn lock(&self) -> Result<Lock, Error> {
        Ok(Lock::new(self.handle)?)
    }

    pub fn cwd(&self) -> Result<String, Error> {
        // TODO: get the CWD.
        // seems a little involved: http://wj32.org/wp/2009/01/24/howto-get-the-command-line-of-processes/
        // steps:
        //      1) NtQueryInformationProcess to get PebBaseAddress, which ProcessParameters
        //          is at some constant offset (+10 on 32 bit etc)
        //      2) ReadProcessMemory to get RTL_USER_PROCESS_PARAMETERS struct
        //      3) get CWD from the struct (has UNICODE_DATA object with ptr + length to CWD)

        let exe = self.exe()?;
        if let Some(parent) =  std::path::Path::new(&exe).parent() {
            return Ok(parent.to_string_lossy().into_owned());
        }
        Ok("/".to_owned())
    }

    pub fn threads(&self) -> Result<Vec<Thread>, Error> {
        let mut ret = Vec::new();
        unsafe {
            // TODO: do we need to CloseHandle the thing returned here?
            let mut thread: HANDLE = std::mem::zeroed();
            while NtGetNextThread(self.handle, thread, MAXIMUM_ALLOWED, 0, 0,
                                  &mut thread as *mut HANDLE) == 0 {
                ret.push(Thread{thread});
            }
        }
        Ok(ret)
    }

    pub fn unwinder(&self) -> Result<unwinder::Unwinder, Error> {
        unwinder::Unwinder::new(self.handle)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle); }
    }
}

impl super::ProcessMemory for Process {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        Ok(self.handle.copy_address(addr, buf)?)
    }
}

#[derive(Eq, PartialEq, Hash, Copy, Clone)]
pub struct Thread {
    thread: ProcessHandle
}

impl Thread {
    pub fn lock(&self) -> Result<ThreadLock, Error> {
        ThreadLock::new(self.thread)
    }

    pub fn id(&self) -> Result<u64, Error> {
        unsafe { Ok(GetThreadId(self.thread) as u64) }
    }

    pub fn active(&self) -> Result<bool, Error> {
        // Getting whether a thread is active or not is suprisingly difficult on windows
        // Let's attempt this by checking if the thread is doing a syscall (like WaitForSingleObject,
        // etc) and if so assume it's idle
        unsafe {
            let mut data = std::mem::zeroed::<THREAD_LAST_SYSCALL_INFORMATION>();
            let ret = NtQueryInformationThread(self.thread, 21,
                &mut data as *mut _ as *mut VOID,
                std::mem::size_of::<THREAD_LAST_SYSCALL_INFORMATION>() as u32,
                NULL as *mut u32);

            // If this call fails (ret == 0), then the thread isn't waiting
            // TODO: maybe inspect the syscall on sucess, and don't return idle for some
            // of them?
            return Ok(ret != 0);
        }
    }
}


pub struct Lock {
    process: ProcessHandle
}

impl Lock {
    pub fn new(process: ProcessHandle) -> Result<Lock, Error> {
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

pub struct ThreadLock {
    thread: ProcessHandle
}

impl ThreadLock {
    pub fn new(thread: ProcessHandle) -> Result<ThreadLock, Error> {
        unsafe {
            let ret = SuspendThread(thread);
            if ret.wrapping_add(1) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }

            Ok(ThreadLock{thread})
        }
    }
}

impl Drop for ThreadLock {
    fn drop(&mut self) {
        unsafe {
            if ResumeThread(self.thread).wrapping_add(1) == 0 {
                panic!("Failed to resume thread {}", std::io::Error::last_os_error());
            }
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct THREAD_LAST_SYSCALL_INFORMATION {
    arg1: PVOID,
    syscall_number: USHORT
}
