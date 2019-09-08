use winapi::um::processthreadsapi::{GetThreadContext, };
use winapi::um::winnt::{HANDLE, CONTEXT, IMAGE_FILE_MACHINE_AMD64};

use winapi::shared::minwindef::TRUE;
use winapi::um::dbghelp::{StackWalk64, STACKFRAME64, AddrModeFlat, ADDRESS64};

use super::Thread;
use super::super::Error;

pub struct Unwinder {
    pub handle: HANDLE
}

pub struct Cursor {
    ctx: Context,
    frame: STACKFRAME64,
    process: HANDLE,
    thread: HANDLE,
}

impl Unwinder {
    pub fn new(handle: HANDLE) -> Result<Unwinder, Error> {
        Ok(Unwinder{handle})
    }

    pub fn cursor(&self, thread: &Thread) -> Result<Cursor, Error> {
        Cursor::new(thread.thread.0, self.handle)
    }
}

impl Cursor {
    pub fn new(thread: HANDLE, process: HANDLE) -> Result<Cursor, Error> {
        unsafe {
            let mut ctx: Context = std::mem::zeroed();
            ctx.0.ContextFlags = 1048587; // CONTEXT_FULL
            if GetThreadContext(thread, &mut ctx.0 as *mut CONTEXT) == 0 {
                return Err(Error::from(std::io::Error::last_os_error()));
            }

            // translate context into stack frame.
            // TODO: if we ever decide to support 32-bit windows this will need extended
            let mut frame: STACKFRAME64 = std::mem::zeroed();
            fn set_flat_addr(addr: &mut ADDRESS64, offset: u64) {
                addr.Offset = offset;
                addr.Mode = AddrModeFlat;
            }
            set_flat_addr(&mut frame.AddrStack, ctx.0.Rsp as u64);
            set_flat_addr(&mut frame.AddrFrame, ctx.0.Rbp as u64);
            set_flat_addr(&mut frame.AddrPC, ctx.0.Rip as u64);

            Ok(Cursor{ctx, frame, thread, process})
        }
    }

    fn unwind(&mut self) -> Result<Option<u64>, Error> {
        unsafe {
            if StackWalk64(IMAGE_FILE_MACHINE_AMD64.into(), self.process, self.thread,
                        &mut self.frame,
                        &mut self.ctx.0 as *mut CONTEXT as *mut _,
                        None, None, None, None) != TRUE {
                return Ok(None);
            }
            Ok(Some(self.ip()))
        }
    }

    pub fn ip(&self) -> u64 { self.frame.AddrPC.Offset }
    pub fn sp(&self) -> u64 { self.frame.AddrStack.Offset }
    pub fn bp(&self) -> u64 { self.frame.AddrFrame.Offset }
}

impl Iterator for Cursor {
    type Item = Result<u64, Error>;

    fn next(&mut self) -> Option<Result<u64, Error>> {
        match self.unwind() {
            Ok(Some(addr)) => Some(Ok(addr)),
            Err(e) => Some(Err(e)),
            Ok(None) => None,
        }
    }
}

#[repr(C, align(16))]
struct Context(CONTEXT);