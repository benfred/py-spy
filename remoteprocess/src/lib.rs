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
#[cfg(target_os = "macos")]
extern crate libproc;

#[cfg(windows)]
extern crate winapi;

#[cfg(unix)]
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


#[cfg(target_os="windows")]
mod windows;
#[cfg(target_os="windows")]
pub use windows::*;


#[cfg(unix)]
mod dwarf_unwind;

extern crate fallible_iterator;

#[derive(Debug)]
pub enum Error {
    NoBinaryForAddress(u64),
    GimliError(gimli::Error),
    GoblinError(::goblin::error::Error),
    IOError(std::io::Error),
    Other(String),
    #[cfg(target_os="linux")]
    LibunwindError(linux::libunwind::Error),
    #[cfg(target_os="linux")]
    NixError(nix::Error),
    #[cfg(target_os="macos")]
    CompactUnwindError(osx::compact_unwind::Error),
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
            #[cfg(target_os="linux")]
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
            #[cfg(target_os="linux")]
            Error::LibunwindError(ref e) => e.description(),
            #[cfg(target_os="linux")]
            Error::NixError(ref e) => e.description(),
            #[cfg(target_os="macos")]
            Error::CompactUnwindError(ref e) => e.description(),
            Error::Other(ref e) => e,
        }
    }

    fn cause(&self) -> Option<&std::error::Error> {
        match *self {
            Error::GimliError(ref e) => Some(e),
            Error::GoblinError(ref e) => Some(e),
            Error::IOError(ref e) => Some(e),
            #[cfg(target_os="linux")]
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

#[cfg(target_os="linux")]
impl From<linux::libunwind::Error> for Error {
    fn from(err: linux::libunwind::Error) -> Error {
        Error::LibunwindError(err)
    }
}

#[cfg(target_os="macos")]
impl From<osx::compact_unwind::Error> for Error {
    fn from(err: osx::compact_unwind::Error) -> Error {
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

// blah. TODO: move this into read_process_memory
pub fn copy_struct<T, P>(addr: usize, process: &P) -> std::io::Result<T>
    where P: read_process_memory::CopyAddress {
    let mut data = vec![0; std::mem::size_of::<T>()];
    process.copy_address(addr, &mut data)?;
    Ok(unsafe { std::ptr::read(data.as_ptr() as *const _) })
}
