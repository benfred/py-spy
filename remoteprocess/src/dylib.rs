// this code is taken from backtrace-rs/src/dylib.rs
// https://github.com/alexcrichton/backtrace-rs/blob/master/src/dylib.rs
//
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

use libc::{c_void, self, c_char};
use std::marker;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};

macro_rules! dlsym {
    (extern {
        $(fn $name:ident($($arg:ident: $t:ty),*) -> $ret:ty;)*
    }) => ($(
        #[allow(non_upper_case_globals)]
        static $name: self::dylib::Symbol<unsafe extern fn($($t),*) -> $ret> =
            self::dylib::Symbol {
                name: concat!(stringify!($name), "\0"),
                addr: ::std::sync::atomic::ATOMIC_USIZE_INIT,
                _marker: ::std::marker::PhantomData,
            };
    )*)
}

pub struct Dylib {
    pub init: AtomicUsize,
}

pub struct Symbol<T> {
    pub name: &'static str,
    pub addr: AtomicUsize,
    pub _marker: marker::PhantomData<T>,
}

impl Dylib {
    pub unsafe fn get<'a, T>(&self, sym: &'a Symbol<T>) -> Option<&'a T> {
        self.load().and_then(|handle| {
            sym.get(handle)
        })
    }

    pub unsafe fn init(&self, path: &str) -> bool {
        if self.init.load(Ordering::SeqCst) != 0 {
            return true
        }
        assert!(path.as_bytes()[path.len() - 1] == 0);
        let ptr = libc::dlopen(path.as_ptr() as *const c_char, libc::RTLD_LAZY);
        if ptr.is_null() {
            return false
        }
        match self.init.compare_and_swap(0, ptr as usize, Ordering::SeqCst) {
            0 => {}
            _ => { libc::dlclose(ptr); }
        }
        return true
    }

    unsafe fn load(&self) -> Option<*mut c_void> {
        match self.init.load(Ordering::SeqCst) {
            0 => None,
            n => Some(n as *mut c_void),
        }
    }
}

impl<T> Symbol<T> {
    unsafe fn get(&self, handle: *mut c_void) -> Option<&T> {
        assert_eq!(mem::size_of::<T>(), mem::size_of_val(&self.addr));
        if self.addr.load(Ordering::SeqCst) == 0 {
            self.addr.store(fetch(handle, self.name.as_ptr()), Ordering::SeqCst)
        }
        if self.addr.load(Ordering::SeqCst) == 1 {
            None
        } else {
            mem::transmute::<&AtomicUsize, Option<&T>>(&self.addr)
        }
    }
}

unsafe fn fetch(handle: *mut c_void, name: *const u8) -> usize {
    let ptr = libc::dlsym(handle, name as *const _);
    if ptr.is_null() {
        1
    } else {
        ptr as usize
    }
}
