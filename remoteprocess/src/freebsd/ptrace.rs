use libc::{c_int, pid_t, c_void, lwpid_t};
use libc::{PT_ATTACH, PT_DETACH, PT_GETREGS};

use std::{ptr};
use std::io::Error;

macro_rules! ptrace {
    ($request:ident, $pid:expr, $addr:expr, $data:expr) => {
        unsafe {
            let ret = ptrace($request, $pid, $addr, $data);

            if ret < 0 {
                return Err(Error::last_os_error());
            }

            ret
        }
    }
}

extern "C" {
    fn ptrace(request: c_int, pid: pid_t,
              data: *const c_void,
              count: c_int) -> c_int;
}


pub fn attach(tid: lwpid_t) -> Result<(), Error> {
    ptrace!(PT_ATTACH, tid, ptr::null(), 0);

    Ok(())
}

pub fn getregs(tid: lwpid_t, addr: *const c_void) -> Result<(), Error> {
    ptrace!(PT_GETREGS, tid, addr, 0);

    Ok(())
}

pub fn detach(tid: lwpid_t) -> Result<(), Error> {
    ptrace!(PT_DETACH, tid, ptr::null(), 0);

    Ok(())
}
