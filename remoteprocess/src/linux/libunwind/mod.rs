use libc::{c_int, c_void, c_char, size_t, pid_t};
use std;

mod bindings;

use self::bindings::{unw_addr_space_t, unw_cursor, unw_accessors_t, unw_cursor_t, unw_regnum_t, unw_word_t,
                     unw_frame_regnum_t_UNW_REG_IP, unw_frame_regnum_t_UNW_REG_SP,
                     unw_caching_policy_t, unw_caching_policy_t_UNW_CACHE_PER_THREAD};

#[allow(non_camel_case_types)]
#[derive(Debug)]
pub enum Error {
    UNW_EUNSPEC,
    UNW_ENOMEM,
    UNW_EBADREG,
    UNW_EREADONLYREG,
    UNW_ESTOPUNWIND,
    UNW_EINVALIDIP,
    UNW_EBADFRAME,
    UNW_EINVAL,
    UNW_EBADVERSION,
    UNW_ENOINFO
}

type Result<T> = std::result::Result<T, crate::Error>;

pub struct Unwinder {
    pub addr_space: unw_addr_space_t
}

impl Unwinder {
    pub fn new() -> Result<Unwinder> {
        unsafe {
            let addr_space = create_addr_space(&_UPT_accessors as *const _ as *mut _, 0);
            // enabling caching provides a modest speedup - but is still much slower than the gimli unwinding
            set_caching_policy(addr_space, unw_caching_policy_t_UNW_CACHE_PER_THREAD);
            Ok(Unwinder{addr_space})
        }
    }

    pub fn cursor(&self, thread: &crate::Thread) -> Result<Cursor> {
        unsafe
        {
            let upt = _UPT_create(thread.id()? as _);
            let mut cursor = std::mem::uninitialized();
            let ret = init_remote(&mut cursor, self.addr_space, upt);
            if ret != 0 {
                return Err(crate::Error::LibunwindError(Error::from(-ret)));
            }
            Ok(Cursor{cursor, upt, initial_frame: true})
        }
    }
}

impl Drop for Unwinder {
    fn drop(&mut self) {
        unsafe {
            destroy_addr_space(self.addr_space);
        }
    }
}

pub struct Cursor {
    cursor: unw_cursor,
    upt: * mut c_void,
    initial_frame: bool
}

impl Cursor {
    pub unsafe fn register(&self, register: i32) -> Result<u64> {
        let mut value = 0;
        let cursor = &self.cursor as *const _ as *mut _;

        match get_reg(cursor, register, &mut value) {
            0 => Ok(value),
            err => Err(crate::Error::LibunwindError(Error::from(-err)))
        }
    }

    pub fn bx(&self) -> Result<u64> {
        unsafe { self.register(3) }
    }

    pub fn ip(&self) -> Result<u64> {
        unsafe { self.register(unw_frame_regnum_t_UNW_REG_IP as i32) }
    }

    pub fn sp(&self) -> Result<u64> {
        unsafe { self.register(unw_frame_regnum_t_UNW_REG_SP as i32) }
    }

    pub fn proc_name(&self) -> Result<String> {
        unsafe {
            let mut name = vec![0_i8; 128];
            let cursor = &self.cursor as *const _ as *mut _;
            let mut raw_offset = std::mem::uninitialized();

            loop {
                match get_proc_name(cursor, name.as_mut_ptr(), name.len(), &mut raw_offset) {
                    0 => break,
                    // TODO: use -UNW_ENOMEM or something instead
                    -2 =>  {
                        let new_length = name.len() * 2;
                        name.resize(new_length, 0);
                        continue;
                    },
                    err => {
                        return Err(crate::Error::LibunwindError(Error::from(-err)));
                    }
                }
            }
            Ok(std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned())
        }
    }
}

impl Iterator for Cursor {
    type Item = Result<u64>;

    fn next(&mut self) -> Option<Result<u64>> {
        // we need to return the initial stack frame, so only call unw_step if
        // this isn't the first frame
        if !self.initial_frame {
            unsafe {
                match step(&mut self.cursor) {
                    0 => return None,
                    err if err < 0 => return Some(Err(crate::Error::LibunwindError(Error::from(-err)))),
                    _ => {}
                }
            };
        } else {
            self.initial_frame = false;
        }

        match self.ip() {
            Ok(0) => None,
            Ok(ip) => Some(Ok(ip)),
            Err(e) => Some(Err(e))
        }
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        unsafe {
            _UPT_destroy(self.upt);
        }
    }
}

extern {
    // functions in libunwind-ptrace.so
    fn _UPT_create(pid: pid_t) -> *mut c_void;
    fn _UPT_destroy(p: *mut c_void) -> c_void;
    #[allow(improper_ctypes)]
    static _UPT_accessors: unw_accessors_t;
}

#[cfg(target_pointer_width="64")]
extern {
    // functions in libunwind-x86_64.so (TODO: define similar for 32bit)
     #[link_name="_Ux86_64_create_addr_space"]
    #[allow(improper_ctypes)]
    fn create_addr_space(acc: *mut unw_accessors_t, byteorder: c_int) -> unw_addr_space_t;
    #[link_name="_Ux86_64_destroy_addr_space"]
    fn destroy_addr_space(addr: unw_addr_space_t) -> c_void;
    #[link_name="_Ux86_64_init_remote"]
    fn init_remote(cursor: *mut unw_cursor_t, addr: unw_addr_space_t, ptr: *mut c_void) -> c_int;
    #[link_name="_Ux86_64_get_reg"]
    fn get_reg(cursor: *mut unw_cursor_t, reg: unw_regnum_t, val: *mut unw_word_t) -> c_int;
    #[link_name="_Ux86_64_step"]
    fn step(cursor: *mut unw_cursor_t) -> c_int;
    #[link_name="_Ux86_64_get_proc_name"]
    fn get_proc_name(cursor: *mut unw_cursor, buffer: * mut c_char, len: size_t, offset: *mut unw_word_t) -> c_int;
    #[link_name="_Ux86_64_set_caching_policy"]
    fn set_caching_policy(spc: unw_addr_space_t, policy: unw_caching_policy_t) -> c_int;
}

#[cfg(target_pointer_width="32")]
extern {
     #[link_name="_Ux86_create_addr_space"]
    fn create_addr_space(acc: *mut unw_accessors_t, byteorder: c_int) -> unw_addr_space_t;
    #[link_name="_Ux86_destroy_addr_space"]
    fn destroy_addr_space(addr: unw_addr_space_t) -> c_void;
    #[link_name="_Ux86_init_remote"]
    fn init_remote(cursor: *mut unw_cursor_t, addr: unw_addr_space_t, ptr: *mut c_void) -> c_int;
    #[link_name="_Ux86_get_reg"]
    fn get_reg(cursor: *mut unw_cursor_t, reg: unw_regnum_t, val: *mut unw_word_t) -> c_int;
    #[link_name="_Ux86_step"]
    fn step(cursor: *mut unw_cursor_t) -> c_int;
    #[link_name="_Ux86_get_proc_name"]
    fn get_proc_name(cursor: *mut unw_cursor, buffer: * mut c_char, len: size_t, offset: *mut unw_word_t) -> c_int;
    #[link_name="_Ux86_set_caching_policy"]
    fn set_caching_policy(spc: unw_addr_space_t, policy: unw_caching_policy_t) -> c_int;
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Error::UNW_EUNSPEC => write!(f, "UNW_EUNSPEC: unspecified (general) error"),
            Error::UNW_ENOMEM => write!(f, "UNW_ENOMEM: out of memoryr"),
            Error::UNW_EBADREG => write!(f, "UNW_EBADREG: bad register number"),
            Error::UNW_EREADONLYREG => write!(f, "UNW_EREADONLYREG: attempt to write read-only register "),
            Error::UNW_ESTOPUNWIND => write!(f, "UNW_ESTOPUNWIND: stop unwinding"),
            Error::UNW_EINVALIDIP => write!(f, "UNW_EINVALIDIP: invalid IP"),
            Error::UNW_EBADFRAME => write!(f, "UNW_EBADFRAME: bad frame"),
            Error::UNW_EINVAL => write!(f, "UNW_EINVAL: unsupported operation or bad value"),
            Error::UNW_EBADVERSION => write!(f, "UNW_EBADVERSION: unwind info has unsupported version"),
            Error::UNW_ENOINFO => write!(f, "UNW_ENOINFO: no unwind info found"),
        }
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        "LibunwindErrror"
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        None
    }
}

impl Error {
    fn from(ret: i32) -> Error {
        // let ret = ret unw_error_t
        match ret as u32 {
            bindings::unw_error_t_UNW_EUNSPEC => Error::UNW_EUNSPEC,
            bindings::unw_error_t_UNW_ENOMEM => Error::UNW_ENOMEM,
            bindings::unw_error_t_UNW_EBADREG => Error::UNW_EBADREG,
            bindings::unw_error_t_UNW_EREADONLYREG => Error::UNW_EREADONLYREG,
            bindings::unw_error_t_UNW_ESTOPUNWIND => Error::UNW_ESTOPUNWIND,
            bindings::unw_error_t_UNW_EINVALIDIP => Error::UNW_EINVALIDIP,
            bindings::unw_error_t_UNW_EBADFRAME => Error::UNW_EBADFRAME,
            bindings::unw_error_t_UNW_EINVAL => Error::UNW_EINVAL,
            bindings::unw_error_t_UNW_EBADVERSION => Error::UNW_EBADVERSION,
            bindings::unw_error_t_UNW_ENOINFO => Error::UNW_ENOINFO,
            _ => Error::UNW_EUNSPEC
        }
    }
}
