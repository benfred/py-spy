//! This crate provides a cross platform way of querying information about other processes running
//! on the system. This let's you build profiling and debugging tools.
//!
//! Features:
//!
//! * Getting the process executable name and current working directory
//! * Listing all the threads in the process
//! * Suspending the execution of a process or thread
//! * Returning if a thread is running or not
//! * Getting a stack trace for a thread in the target process
//! * Resolve symbols for an address in the other process
//! * Copy memory from the other process (using the read_process_memory crate)
//!
//! This crate provides implementations for Linux, OSX and Windows. However this crate is still
//! very much in alpha stage, and the following caveats apply:
//!
//! * Stack unwinding only works on x86_64 processors right now, and is disabled for arm/x86
//! * the OSX stack unwinding code is very unstable and shouldn't be relied on
//! * Getting the cwd on windows returns incorrect results
//!
//! # Example
//!
//! ```rust,no_run
//! #[cfg(unwind)]
//! fn get_backtrace(pid: remoteprocess::Pid) -> Result<(), remoteprocess::Error> {
//!     // Create a new handle to the process
//!     let process = remoteprocess::Process::new(pid)?;
//!     // Create a stack unwind object, and use it to get the stack for each thread
//!     let unwinder = process.unwinder()?;
//!     for thread in process.threads()?.iter() {
//!         println!("Thread {} - {}", thread.id()?, if thread.active()? { "running" } else { "idle" });
//!
//!         // lock the thread to get a consistent snapshot (unwinding will fail otherwise)
//!         // Note: the thread will appear idle when locked, so we are calling
//!         // thread.active() before this
//!         let _lock = thread.lock()?;
//!
//!         // Iterate over the callstack for the current thread
//!         for ip in unwinder.cursor(&thread)? {
//!             let ip = ip?;
//!
//!             // Lookup the current stack frame containing a filename/function/linenumber etc
//!             // for the current address
//!             unwinder.symbolicate(ip, true, &mut |sf| {
//!                 println!("\t{}", sf);
//!             })?;
//!         }
//!     }
//!     Ok(())
//! }
//! ```

extern crate proc_maps;
extern crate goblin;
extern crate benfred_read_process_memory as read_process_memory;
extern crate memmap;
extern crate gimli;
extern crate libc;
#[macro_use]
extern crate log;

#[cfg(target_os="linux")]
extern crate nix;
#[cfg(target_os="linux")]
extern crate object;
#[cfg(target_os="linux")]
extern crate addr2line;

#[cfg(target_os="macos")]
extern crate mach_o_sys;
#[cfg(target_os="macos")]
extern crate mach;
#[cfg(target_os="macos")]
extern crate libproc;

#[cfg(windows)]
extern crate winapi;

#[cfg(all(target_os="macos", unwind))]
#[macro_use]
mod dylib;

#[cfg(target_os="macos")]
mod osx;
#[cfg(target_os="macos")]
pub use osx::*;

#[cfg(target_os="linux")]
mod linux;
#[cfg(target_os="linux")]
pub use linux::*;

#[cfg(target_os="freebsd")]
mod freebsd;
#[cfg(target_os="freebsd")]
pub use freebsd::*;

#[cfg(target_os="windows")]
mod windows;
#[cfg(target_os="windows")]
pub use windows::*;

#[cfg(all(unix, unwind))]
mod dwarf_unwind;

#[derive(Debug)]
pub enum Error {
    NoBinaryForAddress(u64),
    GimliError(gimli::Error),
    GoblinError(::goblin::error::Error),
    IOError(std::io::Error),
    Other(String),
    #[cfg(all(target_os="linux", unwind))]
    LibunwindError(linux::libunwind::Error),
    #[cfg(target_os="linux")]
    NixError(nix::Error),
    #[cfg(target_os="macos")]
    CompactUnwindError(osx::compact_unwind::CompactUnwindError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Error::NoBinaryForAddress(addr) => {
                write!(f, "No binary found for address 0x{:016x}. Try reloading.", addr)
            },
            Error::GimliError(ref e) => e.fmt(f),
            Error::GoblinError(ref e) => e.fmt(f),
            Error::IOError(ref e) => e.fmt(f),
            Error::Other(ref e) => write!(f, "{}", e),
            #[cfg(all(target_os="linux", unwind))]
            Error::LibunwindError(ref e) => e.fmt(f),
            #[cfg(target_os="linux")]
            Error::NixError(ref e) => e.fmt(f),
            #[cfg(target_os="macos")]
            Error::CompactUnwindError(ref e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::NoBinaryForAddress(_) => "No binary found for address",
            Error::GimliError(ref e) => e.description(),
            Error::GoblinError(ref e) => e.description(),
            Error::IOError(ref e) => e.description(),
            #[cfg(all(target_os="linux", unwind))]
            Error::LibunwindError(ref e) => e.description(),
            #[cfg(target_os="linux")]
            Error::NixError(ref e) => e.description(),
            #[cfg(target_os="macos")]
            Error::CompactUnwindError(ref e) => e.description(),
            Error::Other(ref e) => e,
        }
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        match *self {
            Error::GimliError(ref e) => Some(e),
            Error::GoblinError(ref e) => Some(e),
            Error::IOError(ref e) => Some(e),
            #[cfg(all(target_os="linux", unwind))]
            Error::LibunwindError(ref e) => Some(e),
            #[cfg(target_os="linux")]
            Error::NixError(ref e) => Some(e),
            #[cfg(target_os="macos")]
            Error::CompactUnwindError(ref e) => Some(e),
            _ => None,
        }
    }
}

impl From<gimli::Error> for Error {
    fn from(err: gimli::Error) -> Error {
        Error::GimliError(err)
    }
}

impl From<goblin::error::Error> for Error {
    fn from(err: goblin::error::Error) -> Error {
        Error::GoblinError(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::IOError(err)
    }
}

#[cfg(target_os="linux")]
impl From<nix::Error> for Error {
    fn from(err: nix::Error) -> Error {
        Error::NixError(err)
    }
}

#[cfg(all(target_os="linux", unwind))]
impl From<linux::libunwind::Error> for Error {
    fn from(err: linux::libunwind::Error) -> Error {
        Error::LibunwindError(err)
    }
}

#[cfg(target_os="macos")]
impl From<osx::compact_unwind::CompactUnwindError> for Error {
    fn from(err: osx::compact_unwind::CompactUnwindError) -> Error {
        Error::CompactUnwindError(err)
    }
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub line: Option<u64>,
    pub filename: Option<String>,
    pub function: Option<String>,
    pub module: String,
    pub addr: u64
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let function = self.function.as_ref().map(String::as_str).unwrap_or("?");
        if let Some(filename) = self.filename.as_ref() {
            write!(f, "0x{:016x} {} ({}:{})", self.addr, function, filename, self.line.unwrap_or(0))
        } else {
            write!(f, "0x{:016x} {} ({})", self.addr, function, self.module)
        }
    }
}

pub trait ProcessMemory {
    /// Copies memory from another process into an already allocated
    /// byte buffer
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error>;

    /// Copies a series of bytes from another process. Main difference
    /// with 'read' is that this will allocate memory for you
    fn copy(&self, addr: usize, length: usize) -> Result<Vec<u8>, Error> {
        let mut data = vec![0; length];
        self.read(addr, &mut data)?;
        Ok(data)
    }

    /// Copies a structure from another process
    fn copy_struct<T>(&self, addr: usize) -> Result<T, Error> {
        let mut data = vec![0; std::mem::size_of::<T>()];
        self.read(addr, &mut data)?;
        Ok(unsafe { std::ptr::read(data.as_ptr() as *const _) })
    }

    /// Given a pointer that points to a struct in another process, returns the struct
    fn copy_pointer<T>(&self, ptr: *const T) -> Result<T, Error> {
        self.copy_struct(ptr as usize)
    }
}

#[doc(hidden)]
/// Mock for using ProcessMemory on the local process.
pub struct LocalProcess;
impl ProcessMemory for LocalProcess {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), Error> {
        unsafe {
            std::ptr::copy_nonoverlapping(addr as *mut u8, buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }
}


#[cfg(test)]
pub mod tests {
    use super::*;

    struct Point { x: i32, y: i64 }

    #[test]
    fn test_copy_pointer() {
        let original = Point{x:15, y:25};
        let copy = LocalProcess.copy_pointer(&original).unwrap();
        assert_eq!(original.x, copy.x);
        assert_eq!(original.y, copy.y);
    }

    #[test]
    fn test_copy_struct() {
        let original = Point{x:10, y:20};
        let copy: Point = LocalProcess.copy_struct(&original as *const Point as usize).unwrap();
        assert_eq!(original.x, copy.x);
        assert_eq!(original.y, copy.y);
    }
}
