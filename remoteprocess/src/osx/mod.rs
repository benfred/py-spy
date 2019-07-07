pub mod compact_unwind;
mod utils;
mod symbolication;
mod mach_thread_bindings;
mod unwinder;

use std;
use mach;

use super::{ProcessMemory, Error};
use mach::kern_return::{KERN_SUCCESS};
use mach::port::{mach_port_name_t, MACH_PORT_NULL};
use mach::traps::{task_for_pid, mach_task_self};
use read_process_memory::{CopyAddress};

use libc::{c_int, pid_t};

use mach::kern_return::{kern_return_t};
use mach::mach_types::{thread_act_t};
use mach::structs::x86_thread_state64_t;
use mach::thread_status::x86_THREAD_STATE64;
use mach::thread_act::{thread_get_state};

pub use self::utils::{TaskLock, ThreadLock};
pub use self::unwinder::Unwinder;

use libproc::libproc::proc_pid::{pidpath, pidinfo, PIDInfo, PidInfoFlavor};

pub type Pid = pid_t;
pub type Tid = u32;

pub struct Process {
    pub pid: Pid,
    pub task: mach_port_name_t
}

#[derive(Eq, PartialEq, Hash, Copy, Clone)]
pub struct Thread {
    pub tid: Tid
}

impl Process {
    pub fn new(pid: Pid) -> Result<Process, Error> {
        let mut task: mach_port_name_t = MACH_PORT_NULL;
        let result = unsafe { task_for_pid(mach_task_self(), pid as c_int, &mut task) };
        if result != KERN_SUCCESS {
            return Err(Error::IOError(std::io::Error::last_os_error()));
        }
        Ok(Process{pid, task})
    }

    pub fn exe(&self) -> Result<String, Error> {
        pidpath(self.pid).map_err(|e| Error::Other(format!("proc_pidpath failed: {}", e)))
    }

    pub fn cwd(&self) -> Result<String, Error> {
        let cwd = pidinfo::<proc_vnodepathinfo>(self.pid, 0)
            .map_err(|e| Error::Other(format!("proc_pidinfo failed: {}", e)))?;
        Ok(unsafe { std::ffi::CStr::from_ptr(cwd.pvi_cdir.vip_path.as_ptr()) }.to_string_lossy().to_string())
    }

    pub fn lock(&self) -> Result<TaskLock, Error> {
        Ok(TaskLock::new(self.task)?)
    }

    pub fn threads(&self) -> Result<Vec<Thread>, Error> {
        let mut threads: mach::mach_types::thread_act_array_t = unsafe { std::mem::zeroed() };
        let mut thread_count: u32 = 0;
        let result = unsafe { mach::task::task_threads(self.task, &mut threads, &mut thread_count) };
        if result != KERN_SUCCESS {
            return Err(Error::IOError(std::io::Error::last_os_error()));
        }

        let mut ret = Vec::new();
        for i in 0..thread_count {
            let tid = unsafe { *threads.offset(i as isize) };
            ret.push(Thread{tid});
        }
        Ok(ret)
    }

    pub fn unwinder(&self) -> Result<Unwinder, Error> {
        Ok(Unwinder::new(self.pid)?)
    }
}

impl super::ProcessMemory for Process {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        Ok(self.task.copy_address(addr, buf)?)
    }
}

use self::mach_thread_bindings::{thread_info, thread_basic_info, thread_identifier_info,
                                 THREAD_IDENTIFIER_INFO, THREAD_BASIC_INFO,
                                 TH_FLAGS_IDLE, TH_STATE_RUNNING};


impl Thread {
    pub fn new(tid: Tid) -> Result<Thread, Error> {
        Ok(Thread{tid})
    }

    pub fn id(&self) -> Result<Tid, Error> {
        Ok(self.tid)
    }

    pub fn thread_handle(&self) -> Result<u64, Error> {
        let thread_id = self.get_thread_identifier_info()?;
        Ok(thread_id.thread_handle)
    }

    pub fn active(&self) -> Result<bool, Error> {
        let info = self.get_thread_basic_info()?;
        Ok(info.run_state == TH_STATE_RUNNING as i32 &&
           info.flags & TH_FLAGS_IDLE as i32 == 0)
    }

    pub fn lock(&self) -> Result<ThreadLock, Error> {
        Ok(ThreadLock::new(self.tid)?)
    }

    pub fn registers(&self) -> Result<x86_thread_state64_t, std::io::Error> {
        unsafe {
            let thread_state = x86_thread_state64_t::new();
            let thread_state_size = x86_thread_state64_t::count();
            let result = thread_get_state(self.tid, x86_THREAD_STATE64, std::mem::transmute(&thread_state),
                                        std::mem::transmute(&thread_state_size));
            if result != KERN_SUCCESS {
                return Err(std::io::Error::last_os_error());
            }
            Ok(thread_state)
        }
    }

    pub fn get_thread_basic_info(&self) -> Result<thread_basic_info, std::io::Error> {
        let mut info: thread_basic_info = unsafe { std::mem::zeroed() };
        let mut info_size: u32 = (std::mem::size_of::<thread_basic_info>() / std::mem::size_of::<i32>()) as u32;

        let result = unsafe {
            thread_info(self.tid, THREAD_BASIC_INFO,
                        &mut info as *mut thread_basic_info as *mut i32,
                        &mut info_size)
        };
        if result != KERN_SUCCESS {
            return Err(std::io::Error::last_os_error());
        }
        Ok(info)
    }

    pub fn get_thread_identifier_info(&self) -> Result<thread_identifier_info, std::io::Error> {
        let mut thread_id: thread_identifier_info = unsafe { std::mem::zeroed() };
        let mut thread_id_size: u32 = (std::mem::size_of::<thread_identifier_info>() / std::mem::size_of::<i32>()) as u32;
        let result = unsafe {
            thread_info(self.tid,
                        THREAD_IDENTIFIER_INFO,
                        &mut thread_id as *mut thread_identifier_info as *mut i32,
                        &mut thread_id_size)
        };
        if result != KERN_SUCCESS {
            return Err(std::io::Error::last_os_error());
        }
        Ok(thread_id)
    }
}

// extra struct definitions needed to get CWD from proc_pidinfo
#[repr(C)]
#[derive(Copy, Clone)]
struct vnode_info_path {
    _opaque: [::std::os::raw::c_char; 152],
    pub vip_path: [::std::os::raw::c_char; 1024],
}
#[repr(C)]
#[derive(Copy, Clone)]
struct proc_vnodepathinfo {
    pub pvi_cdir: vnode_info_path,
    pub pvi_rdir: vnode_info_path,
}
impl Default for proc_vnodepathinfo {
    fn default() -> Self {
        unsafe { ::std::mem::zeroed() }
    }
}
impl PIDInfo for proc_vnodepathinfo {
    fn flavor() -> PidInfoFlavor { PidInfoFlavor::VNodePathInfo }
}


