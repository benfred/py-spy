use libc::{c_char, c_int, c_void, pid_t};

use super::kinfo_proc::kinfo_proc;
use std::io::Error;
use std::ffi::CStr;

// Executable path buffer size
const BUF_SIZE: usize = 4096;
// File info flag. Designates that file is CWD.
const PS_FST_UFLAG_CDIR: c_int = 0x0002;
const KERN_PROC_PID: c_int = 1;
const KERN_PROC_PROC: c_int = 8;
const KERN_PROC_INC_THREAD: c_int = 0x10;

#[link(name="procstat")]
extern "C" {
    fn procstat_open_sysctl() -> *const c_void;
    fn procstat_getprocs(prstat: *const c_void,
                         request: c_int,
                         pid: pid_t,
                         count: *const c_int) -> *const kinfo_proc;
    fn procstat_getpathname(prstat: *const c_void,
                            kinfo_proc: *const kinfo_proc,
                            pathname: *const c_char,
                            size: c_int);
    fn procstat_getfiles(prstat: *const c_void,
                         kinfo_proc: *const kinfo_proc,
                         mmap: c_int) -> *const stailq_entry;
    fn procstat_freeprocs(prstat: *const c_void,
                          kinfo_proc: *const kinfo_proc);
    fn procstat_close(prstat: *const c_void);
}

#[derive(Debug)]
#[repr(C)]
struct Filestat {
    fs_type: c_int,
    fs_flags: c_int,
    fs_fflags: c_int,
    fs_uflags: c_int,
    fs_fd: c_int,
    fs_ref_count: c_int,
    fs_offset: c_int,
    fs_typedep: *const c_void,
    fs_path: *const c_char,
    next: stailq_entry,
}

#[derive(Debug)]
#[repr(C)]
struct stailq_entry {
    next: *const Filestat,
}

fn procstat_call<T>(
    request: c_int,
    pid: pid_t,
    count: c_int,
    call: &dyn Fn(*const c_void, *const kinfo_proc, c_int) -> T
) -> Result<T, Error> {
    unsafe {
        let prstat = procstat_open_sysctl();

        if prstat.is_null() {
            return Err(Error::last_os_error());
        }

        let kinfo_procs =
            procstat_getprocs(prstat, request, pid, &count as *const _);

        if kinfo_procs.is_null() {
            return Err(Error::last_os_error());
        }

        let ret = call(prstat, kinfo_procs, count);

        procstat_freeprocs(prstat, kinfo_procs);
        procstat_close(prstat);

        Ok(ret)
    }
}

/// Retrieves process information via libprocstat
pub fn threads_info(pid: pid_t) -> Result<Vec<kinfo_proc>, Error> {
    let count: c_int = 0;

    procstat_call(KERN_PROC_PID | KERN_PROC_INC_THREAD, pid, count, &|_, kinfo, count| {
        unsafe {
            Ok(std::slice::from_raw_parts(kinfo, count as usize).into())
        }
    })?
}

pub fn exe(pid: pid_t) -> Result<String, Error> {
    let result: [c_char; BUF_SIZE] = [0; BUF_SIZE];

    procstat_call(KERN_PROC_PID | KERN_PROC_INC_THREAD, pid, BUF_SIZE as c_int, &|prstat, kinfo, _| {
        unsafe {
            procstat_getpathname(prstat, kinfo, &result as _, BUF_SIZE as c_int);
            let bytes = CStr::from_ptr(&result as _).to_bytes();

            Ok(String::from_utf8_unchecked(bytes.to_vec()))
        }
    })?
}

fn get_file_with_uflag(pid: pid_t, uflag: c_int)
                       -> Result<Option<String>, Error> {
    procstat_call(KERN_PROC_PID | KERN_PROC_INC_THREAD, pid, 0 as c_int, &|prstat, kinfo, _| {
        let mut filestat = unsafe {
            (*procstat_getfiles(prstat, kinfo, 0)).next
        };

        loop {
            if filestat.is_null() {
                return None;
            };

            unsafe {
                let ref derefered = *filestat;

                if derefered.fs_uflags & uflag != 0 {
                    let bytes = CStr::from_ptr(derefered.fs_path).to_bytes();
                    return Some(String::from_utf8_unchecked(bytes.to_vec()));
                }

                filestat = (derefered.next).next;
            };
        }
    })
}

pub fn cwd(pid: pid_t) -> Result<String, Error> {
    match get_file_with_uflag(pid, PS_FST_UFLAG_CDIR)? {
        Some(string) => Ok(string),
        None => Ok("".to_owned()),
    }
}

pub fn processes() -> Result<std::collections::HashMap<pid_t, pid_t>, Error> {
    procstat_call(KERN_PROC_PROC, 0, 0, &|prstat, kinfo, count| {
        let mut ret = std::collections::HashMap::new();
        let proc = unsafe { std::slice::from_raw_parts(kinfo, count as usize) };
        for proc in proc {
            ret.insert(proc.ki_pid, proc.ki_ppid);
        }
        Ok(ret)
    })?
}
