use winapi::um::winnt::{HANDLE, WCHAR};
use winapi::um::errhandlingapi::GetLastError;
use winapi::shared::minwindef::{TRUE, BOOL, DWORD, MAX_PATH};
use winapi::shared::guiddef::GUID;
use winapi::shared::basetsd::DWORD64;
use winapi::um::dbghelp::{SymInitializeW, SymCleanup,
                          SymFromAddrW, SymGetLineFromAddrW64, MAX_SYM_NAME, SYMBOL_INFOW, IMAGEHLP_LINEW64};
use std::os::windows::ffi::{OsStringExt};

use super::super::Error;
use super::super::StackFrame;
use libc::wcslen;

pub struct Symbolicator {
    pub handle: HANDLE
}

impl Symbolicator {
    pub fn new(handle: HANDLE) -> Result<Symbolicator, Error> {
        unsafe {
            if SymInitializeW(handle, std::ptr::null_mut(), TRUE) == 0 {
                return Err(Error::from(std::io::Error::last_os_error()));
            };
            Ok(Symbolicator{handle})
        }
    }

    pub fn reload(&mut self) -> Result<(), Error> {
        info!("reloading symbol module list");
        unsafe { SymRefreshModuleList(self.handle); }
        Ok(())
    }

    pub fn symbolicate(&self, addr: u64, line_info: bool, callback: &mut dyn FnMut(&StackFrame)) -> Result<(), Error> {
        let function = unsafe { self.symbol_function(addr) };

        let module = match unsafe { self.symbol_module (addr) } {
            Ok(module) => module,
            Err(Error::NoBinaryForAddress(_)) => {
                unsafe {
                    SymRefreshModuleList(self.handle);
                    self.symbol_module(addr).unwrap_or_else(|_| "?".to_owned())
                }
            },
            Err(_) => "?".to_owned()
        };

        let mut line = None;
        let mut filename = None;

        if line_info {
            if let Some((f, l)) = unsafe { self.symbol_filename(addr) } {
                line = Some(l);
                filename = Some(f);
            }
        }
        callback(&StackFrame{function, filename, line, module, addr});
        Ok(())
    }

    // returns the corresponding function name for an address
    pub unsafe fn symbol_function(&self, addr: u64) -> Option<String> {
        let mut buffer = std::mem::zeroed::<SymbolBuffer>();
        let symbol_info = &mut *(buffer.buffer.as_mut_ptr() as *mut SYMBOL_INFOW);
        symbol_info.MaxNameLen = MAX_SYM_NAME as u32;
        // there must be a way to get this
        symbol_info.SizeOfStruct = 88;

        let mut displacement = 0;
        let ret = SymFromAddrW(self.handle, addr, &mut displacement, symbol_info);
        if ret != TRUE {
            return None;
        }

        let length = std::cmp::min(symbol_info.NameLen as usize, symbol_info.MaxNameLen as usize - 1);
        let symbol = std::slice::from_raw_parts(symbol_info.Name.as_ptr() as *const u16, length);
        let symbol = std::ffi::OsString::from_wide(symbol);
        Some(symbol.to_string_lossy().to_owned().to_string())
    }

    // get the corresponding filename/linke
    pub unsafe fn symbol_filename(&self, addr: u64) -> Option<(String, u64)> {
        let mut displacement = 0;
        let mut info = std::mem::zeroed::<IMAGEHLP_LINEW64>();
        info.SizeOfStruct = std::mem::size_of_val(&info) as u32;
        if SymGetLineFromAddrW64(self.handle, addr, &mut displacement, &mut info) != TRUE {
            return None;
        }
        let filename = std::slice::from_raw_parts(info.FileName, wcslen(info.FileName));
        let filename = std::ffi::OsString::from_wide(filename);
        Some((filename.to_string_lossy().to_owned().to_string(), info.LineNumber.into()))
    }

    // get the corresponding module name
    pub unsafe fn symbol_module(&self, addr: u64) -> Result<String, Error> {
        let mut info = std::mem::zeroed::<IMAGEHLP_MODULEW64>();
        info.SizeOfStruct = std::mem::size_of_val(&info) as u32;
        if SymGetModuleInfoW64(self.handle, addr, &mut info) != TRUE {
            if GetLastError() == 126 {
                return Err(Error::NoBinaryForAddress(addr));
            }
            return Err(Error::IOError(std::io::Error::last_os_error()));
        }
        let filename = info.LoadedImageName.as_ptr();
        let filename = std::slice::from_raw_parts(filename, wcslen(filename));
        let filename = std::ffi::OsString::from_wide(filename);

        Ok(filename.to_string_lossy().to_owned().to_string())
    }
}

impl Drop for Symbolicator {
    fn drop(&mut self) {
        unsafe { SymCleanup(self.handle); }
    }
}

#[repr(C, align(8))]
struct SymbolBuffer {
    buffer: [u8; std::mem::size_of::<SYMBOL_INFOW>() + MAX_SYM_NAME * 2]
}

// missing from winapi-rs =(
#[allow(dead_code)]
#[repr(C)]
pub enum SYM_TYPE {
    SymNone,
    SymCoff,
    SymCv,
    SymPdb,
    SymExport,
    SymDeferred,
    SymSym,
    SymDia,
    SymVirtual,
    NumSymTypes,
}

#[allow(non_snake_case)]
#[repr(C)]
pub struct IMAGEHLP_MODULEW64 {
    pub SizeOfStruct: DWORD,
    pub BaseOfImage: DWORD64,
    pub ImageSize: DWORD,
    pub TimeDateStamp: DWORD,
    pub CheckSum: DWORD,
    pub NumSyms: DWORD,
    pub SymType: SYM_TYPE,
    pub ModuleName: [WCHAR; 32],
    pub ImageName: [WCHAR; 256],
    pub LoadedImageName: [WCHAR; 256],
    pub LoadedPdbName: [WCHAR; 256],
    pub CVSig: DWORD,
    pub CVData: [WCHAR; MAX_PATH * 3],
    pub PdbSig: DWORD,
    pub PdbSig70: GUID,
    pub PdbAge: DWORD,
    pub PdbUnmatched: BOOL,
    pub DbgUnmatched: BOOL,
    pub LineNumbers: BOOL,
    pub GlobalSymbols: BOOL,
    pub TypeInfo: BOOL,
    pub SourceIndexed: BOOL,
    pub Publics: BOOL,
    pub MachineType: DWORD,
    pub Reserved: DWORD,
}

#[link(name="dbghelp")]
extern "system" {
    fn SymGetModuleInfoW64(process: HANDLE, addr: u64, info: *mut IMAGEHLP_MODULEW64) -> BOOL;
    fn SymRefreshModuleList(process: HANDLE) -> BOOL;
}
