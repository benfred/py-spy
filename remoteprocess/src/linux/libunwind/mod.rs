/// On linux, the most reliable way of unwinding a stack trace is going to be to use the libunwind-ptrace library
/// However, this isn't guaranteed to be installed on each system - and it doesn't seem like static linking it
/// is a viable solution.
///
/// Also the performance of libunwind-ptrace seems to be quite a bit worse than that of the gimli based unwinder
/// we're using here. (around 10x slower when I was testing on my system)
///
/// So instead of linking directly to libunwind and adding a hard dependency, let's load up at runtime instead.
/// (currently we're using libunwind mainly to validate the gimli unwider)
use std::sync::atomic::ATOMIC_USIZE_INIT;
use libc::{c_int, c_void, c_char, size_t, pid_t};
use dylib::{self, Dylib, Symbol as DylibSymbol};
use std;

mod bindings;

use self::bindings::{unw_addr_space_t, unw_cursor, unw_accessors_t, unw_cursor_t, unw_regnum_t, unw_word_t,
                     unw_frame_regnum_t_UNW_REG_IP, unw_frame_regnum_t_UNW_REG_SP,
                     unw_caching_policy_t, unw_caching_policy_t_UNW_CACHE_PER_THREAD};

#[derive(Debug)]
pub enum Error {
    /// Failed to find one of the required libraries (libunwind.so, libunwind-ptrace.so etc)
    MissingLibrary(&'static str),

    /// Installed library is missing a required symbol
    MissingSymbol(&'static str, &'static str),

    /// libunwind call returned an error value
    LibunwindError(i32),
}

type Result<T> = std::result::Result<T, Error>;

pub struct LibUnwind {
    pub addr_space: unw_addr_space_t
}

impl LibUnwind {
    pub fn new() -> Result<LibUnwind> {
        unsafe {
            // I have a debug build of libunwind here for when things *really* go wrong
            // if !LIBUNWIND_X86_64.init("/home/ben/code/libunwind/dist/lib/libunwind-x86_64.so\0") {
            if !LIBUNWIND_X86_64.init("libunwind-x86_64.so\0") {
                return Err(Error::MissingLibrary("libunwind-x86_64.so"));
            }
            if !LIBUNWIND_PTRACE.init("libunwind-ptrace.so\0") {
                return Err(Error::MissingLibrary("libunwind-ptrace.so"));
            }

            let upt_accessors = ptrace_sym(&UPT_ACCESSORS)?;
            let create_addr_space = x86_64_sym(&_Ux86_64_create_addr_space)?;
            let addr_space = create_addr_space(*upt_accessors as *const _ as *mut _, 0);

            // enabling caching provides a modest speedup - but is still much slower than the gimli unwinding
            x86_64_sym(&_Ux86_64_set_caching_policy)?(addr_space, unw_caching_policy_t_UNW_CACHE_PER_THREAD);

            Ok(LibUnwind{addr_space})
        }
    }

    pub fn cursor(&self, pid: pid_t) -> Result<Cursor> {
        unsafe
        {
            let upt = ptrace_sym(&_UPT_create)?(pid);
            let mut cursor = std::mem::uninitialized();
            let ret = x86_64_sym(&_Ux86_64_init_remote)?(&mut cursor, self.addr_space, upt);
            if ret != 0 {
                return Err(Error::LibunwindError(ret));
            }
            Ok(Cursor{cursor, upt, initial_frame: true})
        }
    }
}

impl Drop for LibUnwind {
    fn drop(&mut self) {
        unsafe {
            x86_64_sym(&_Ux86_64_destroy_addr_space).unwrap()(self.addr_space);
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

        let get_reg = x86_64_sym(&_Ux86_64_get_reg)?;
        match get_reg(cursor, register, &mut value) {
            0 => Ok(value),
            err => Err(Error::LibunwindError(err))
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

            let get_proc_name = x86_64_sym(&_Ux86_64_get_proc_name)?;
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
                        return Err(Error::LibunwindError(err));
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
                let unw_step = match x86_64_sym(&_Ux86_64_step) {
                    Ok(f) => f,
                    Err(e) => return Some(Err(e))
                };
                match unw_step(&mut self.cursor) {
                    0 => return None,
                    err if err < 0 => return Some(Err(Error::LibunwindError(err))),
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
            ptrace_sym(&_UPT_destroy).unwrap()(self.upt);
        }
    }
}

static LIBUNWIND_X86_64: Dylib = Dylib { init: ATOMIC_USIZE_INIT };
static LIBUNWIND_PTRACE: Dylib = Dylib { init: ATOMIC_USIZE_INIT };

dlsym! {
    extern {
        // functions in libunwind-ptrace.so
        fn _UPT_create(pid: pid_t) -> *mut c_void;
        fn _UPT_destroy(p: *mut c_void) -> c_void;

        // functions in libunwind-x86_64.so (TODO: define similar for 32bit)
        fn _Ux86_64_create_addr_space(acc: *mut unw_accessors_t, byteorder: c_int) -> unw_addr_space_t;
        fn _Ux86_64_destroy_addr_space(addr: unw_addr_space_t) -> c_void;
        fn _Ux86_64_init_remote(cursor: *mut unw_cursor_t, addr: unw_addr_space_t, ptr: *mut c_void) -> c_int;
        fn _Ux86_64_get_reg(cursor: *mut unw_cursor_t, reg: unw_regnum_t, val: *mut unw_word_t) -> c_int;
        fn _Ux86_64_step(cursor: *mut unw_cursor_t) -> c_int;
        fn _Ux86_64_get_proc_name(cursor: *mut unw_cursor, buffer: * mut c_char, len: size_t, offset: *mut unw_word_t) -> c_int;
        fn _Ux86_64_set_caching_policy(spc: unw_addr_space_t, policy: unw_caching_policy_t) -> c_int;

    }
}

unsafe fn ptrace_sym<T>(sym: &DylibSymbol<T>) -> Result<&T> {
    match LIBUNWIND_PTRACE.get(sym) {
        Some(val) => Ok(val),
        None => Err(Error::MissingSymbol("libunwind-ptrace.so", &sym.name[..sym.name.len()-1]))
    }
}

unsafe fn x86_64_sym<T>(sym: &DylibSymbol<T>) -> Result<&T> {
    match LIBUNWIND_X86_64.get(sym) {
        Some(val) => Ok(val),
        None => Err(Error::MissingSymbol("libunwind-x86_64.so", &sym.name[..sym.name.len()-1]))
    }
}

static UPT_ACCESSORS: DylibSymbol<& mut unw_accessors_t> = DylibSymbol{
    name: "_UPT_accessors\0",
    addr: ::std::sync::atomic::ATOMIC_USIZE_INIT,
    _marker: ::std::marker::PhantomData,
};

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Error::MissingLibrary(lib) => write!(f, "Missing library {}", lib),
            Error::MissingSymbol(lib, sym) =>  write!(f, "Missing symbol {} from {}", sym, lib),
            Error::LibunwindError(e) => write!(f, "libunwind error {}", e)
        }
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::MissingLibrary(_) => "Missing Library",
            Error::MissingSymbol(_, _) => "Missing Symbol",
            Error::LibunwindError(_) => "LibunwindError"
        }
    }

    fn cause(&self) -> Option<&std::error::Error> {
        None
    }
}