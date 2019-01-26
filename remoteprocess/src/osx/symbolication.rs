// Code to connect to CoreSymbolication library taken from backtrace-rs:
// https://github.com/alexcrichton/backtrace-rs/blob/master/src/symbolize/coresymbolication.rs
// backtrace-rs isn't targetting remote processes, so we can't just use as a dependency,
// however it's relatively trivial to extract the needed bits and make it work with
// a remote process
// backtrace-rs is licensed under the MIT license:

/*
Copyright (c) 2014 Alex Crichton

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
*/

use std::sync::atomic::ATOMIC_USIZE_INIT;

use libc::{c_char, c_int, c_void};
use dylib::{self, Dylib, Symbol as DylibSymbol};

#[repr(C)]
#[derive(Copy, Clone, PartialEq)]
pub struct CSTypeRef {
    cpp_data: *const c_void,
    cpp_obj: *const c_void
}

const CS_NOW: u64 = 0x80000000;
const CSREF_NULL: CSTypeRef = CSTypeRef {
    cpp_data: 0 as *const c_void,
    cpp_obj: 0 as *const c_void,
};

static CORESYMBOLICATION: Dylib = Dylib { init: ATOMIC_USIZE_INIT };

dlsym! {
    extern {
        fn CSSymbolicatorCreateWithPid(pid: c_int) -> CSTypeRef;
        fn CSRelease(rf: CSTypeRef) -> c_void;
        fn CSSymbolicatorGetSymbolWithAddressAtTime(
            cs: CSTypeRef, addr: *const c_void, time: u64) -> CSTypeRef;
        fn CSSymbolicatorGetSourceInfoWithAddressAtTime(
            cs: CSTypeRef, addr: *const c_void, time: u64) -> CSTypeRef;
        fn CSSourceInfoGetLineNumber(info: CSTypeRef) -> c_int;
        fn CSSourceInfoGetPath(info: CSTypeRef) -> *const c_char;
        fn CSSourceInfoGetSymbol(info: CSTypeRef) -> CSTypeRef;
        fn CSSymbolGetMangledName(sym: CSTypeRef) -> *const c_char;
        // fn CSSymbolGetSymbolOwner(sym: CSTypeRef) -> CSTypeRef;
        // fn CSSymbolOwnerGetBaseAddress(symowner: CSTypeRef) -> *mut c_void;
    }
}

unsafe fn get<T>(sym: &DylibSymbol<T>) -> &T {
    CORESYMBOLICATION.get(sym).unwrap()
}

pub struct CoreSymbolication {
    cs: CSTypeRef
}

use std;

pub struct Symbol {
    filename: *const c_char,
    name: *const c_char,
    pub lineno: u32,
}

impl Symbol {
    pub fn name(&self) -> Option<std::ffi::CString> {
        if !self.name.is_null() {
            Some(unsafe { std::ffi::CStr::from_ptr(self.name) }.to_owned())
        } else {
           None
        }
    }

    pub fn filename(&self) -> Option<std::ffi::CString> {
        if !self.filename.is_null() {
            Some(unsafe { std::ffi::CStr::from_ptr(self.filename) }.to_owned())
        } else {
           None
        }
    }
}


impl CoreSymbolication {
   pub unsafe fn new(pid: c_int) -> Option<CoreSymbolication> {
        let path = "/System/Library/PrivateFrameworks/CoreSymbolication.framework\
                /Versions/A/CoreSymbolication\0";

        if !CORESYMBOLICATION.init(path) {
            // TODO: return an error here instead
            return None;
        }

        let cs = get(&CSSymbolicatorCreateWithPid)(pid);
        if cs == CSREF_NULL {
            return None;
        }

        Some(CoreSymbolication{cs})
    }

    pub unsafe fn resolve(&self, addr: u64) -> Option<Symbol> {
        let addr = addr as *const c_void;
        let info = get(&CSSymbolicatorGetSourceInfoWithAddressAtTime)(self.cs, addr, CS_NOW);

        let sym = if info == CSREF_NULL {
            get(&CSSymbolicatorGetSymbolWithAddressAtTime)(self.cs, addr, CS_NOW)
        } else {
            get(&CSSourceInfoGetSymbol)(info)
        };

        if sym == CSREF_NULL {
            return None;
        }

        let lineno = if info != CSREF_NULL {
            get(&CSSourceInfoGetLineNumber)(info) as u32
        } else {
            0
        };

        let filename = if info != CSREF_NULL {
            get(&CSSourceInfoGetPath)(info)
        } else {
            std::ptr::null()
        };
        let name = get(&CSSymbolGetMangledName)(sym);
        Some(Symbol{name, lineno, filename})
    }
}

impl Drop for CoreSymbolication {
    fn drop(&mut self) {
        unsafe {
            get(&CSRelease)(self.cs);
        }
    }
}
