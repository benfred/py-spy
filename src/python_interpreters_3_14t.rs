/* Minimal CPython 3.14 free-threaded interpreter layout.

This intentionally lives outside the generated v3_14_0 bindings.  CPython's
free-threaded ABI changes the PyObject/PyVarObject header layout, so sharing the
normal 3.14 PyUnicodeObject/PyBytesObject bindings corrupts string and line
table reads.  The offsets below are the small subset needed for basic stack
profiling and come from CPython 3.14t headers/debug offsets.
*/

use std::os::raw::{c_char, c_ulong};

use crate::python_interpreters::{
    is_py314_entry_frame_owner, stackref_as_object, BytesObject, CodeObject, FrameObject,
    InterpreterState, ListObject, Object, StringObject, ThreadState, TupleObject, TypeObject,
};

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyInterpreterState;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyThreadState {
    pub prev: *mut PyThreadState,
    pub next: *mut PyThreadState,
    pub interp: *mut PyInterpreterState,
    _pad0: [u8; 48],
    pub current_frame: *mut PyInterpreterFrame,
    _pad1: [u8; 72],
    pub thread_id: u64,
    pub native_thread_id: u64,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyInterpreterFrame {
    pub f_executable: PyStackRef,
    pub previous: *mut PyInterpreterFrame,
    _pad0: [u8; 40],
    pub instr_ptr: *mut u8,
    _pad1: [u8; 14],
    pub owner: c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyStackRef {
    pub bits: usize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyCodeObject {
    _pad0: [u8; 68],
    pub co_argcount: i32,
    _pad1: [u8; 12],
    pub co_firstlineno: i32,
    _pad2: [u8; 24],
    pub co_localsplusnames: *mut PyTupleObject,
    _pad3: [u8; 8],
    pub co_filename: *mut PyUnicodeObject,
    pub co_name: *mut PyUnicodeObject,
    pub co_qualname: *mut PyUnicodeObject,
    pub co_linetable: *mut PyBytesObject,
    _pad4: [u8; 72],
    pub co_code_adaptive: [u8; 1],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyObject {
    _pad0: [u8; 24],
    pub ob_type: *mut PyTypeObject,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyTypeObject {
    _pad0: [u8; 40],
    pub tp_name: *const c_char,
    _pad1: [u8; 136],
    pub tp_flags: c_ulong,
    _pad2: [u8; 112],
    pub tp_dictoffset: isize,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyBytesObject {
    _pad0: [u8; 32],
    pub ob_size: isize,
    _pad1: [u8; 8],
    pub ob_sval: [c_char; 1],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyUnicodeObject {
    _pad0: [u8; 32],
    pub length: isize,
    pub hash: isize,
    pub interned: u8,
    pub state: u8,
    _pad1: [u8; 6],
    pub ascii_data: [u8; 16],
    pub data: *mut std::ffi::c_void,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyTupleObject {
    _pad0: [u8; 32],
    pub ob_size: isize,
    _pad1: [u8; 8],
    pub ob_item: [*mut PyObject; 1],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct PyListObject {
    _pad0: [u8; 32],
    pub ob_size: isize,
    pub ob_item: *mut *mut PyObject,
}

impl InterpreterState for PyInterpreterState {
    type ThreadState = PyThreadState;
    type Object = PyObject;
    type StringObject = PyUnicodeObject;
    type ListObject = PyListObject;
    type TupleObject = PyTupleObject;
    const HAS_GIL_RUNTIME_STATE: bool = true;

    fn threadstate_ptr_ptr(interpreter_address: usize) -> *const *const Self::ThreadState {
        (interpreter_address + 7344) as *const *const Self::ThreadState
    }

    fn modules_ptr_ptr(interpreter_address: usize) -> *const *const Self::Object {
        (interpreter_address + 7712) as *const *const Self::Object
    }
}

impl ThreadState for PyThreadState {
    type FrameObject = PyInterpreterFrame;
    type InterpreterState = PyInterpreterState;

    fn interp(&self) -> *mut Self::InterpreterState {
        self.interp
    }

    fn frame_address(&self) -> Option<usize> {
        None
    }

    fn frame(&self, _offset: Option<usize>) -> *mut Self::FrameObject {
        self.current_frame
    }

    fn thread_id(&self) -> u64 {
        self.thread_id
    }

    fn native_thread_id(&self) -> Option<u64> {
        Some(self.native_thread_id)
    }

    fn next(&self) -> *mut Self {
        self.next
    }
}

impl FrameObject for PyInterpreterFrame {
    type CodeObject = PyCodeObject;

    fn code(&self) -> *mut Self::CodeObject {
        stackref_as_object(self.f_executable.bits, 1) as *mut PyCodeObject
    }

    fn lasti(&self) -> i32 {
        let code = stackref_as_object(self.f_executable.bits, 1) as *const u8;
        unsafe { self.instr_ptr.cast_const().offset_from(code) as i32 }
    }

    fn back(&self) -> *mut Self {
        self.previous
    }

    fn is_entry(&self) -> bool {
        is_py314_entry_frame_owner(self.owner)
    }
}

impl CodeObject for PyCodeObject {
    type BytesObject = PyBytesObject;
    type StringObject = PyUnicodeObject;
    type TupleObject = PyTupleObject;

    fn name(&self) -> *mut Self::StringObject {
        self.co_name
    }

    fn filename(&self) -> *mut Self::StringObject {
        self.co_filename
    }

    fn qualname(&self) -> Option<*mut Self::StringObject> {
        Some(self.co_qualname)
    }

    fn line_table(&self) -> *mut Self::BytesObject {
        self.co_linetable
    }

    fn first_lineno(&self) -> i32 {
        self.co_firstlineno
    }

    fn nlocals(&self) -> i32 {
        0
    }

    fn argcount(&self) -> i32 {
        self.co_argcount
    }

    fn varnames(&self) -> *mut Self::TupleObject {
        self.co_localsplusnames
    }

    fn get_line_number(&self, lasti: i32, table: &[u8]) -> i32 {
        const CO_CODE_ADAPTIVE_OFFSET: i32 = 232;
        let lasti = lasti - CO_CODE_ADAPTIVE_OFFSET;
        let mut line_number = self.first_lineno();
        let mut bytecode_address = 0;
        let mut index = 0;

        loop {
            if index >= table.len() {
                break;
            }
            let byte = table[index];
            index += 1;

            let delta = ((byte & 7) as i32) + 1;
            bytecode_address += delta * 2;
            let code = (byte >> 3) & 15;
            let line_delta = match code {
                15 => 0,
                14 => {
                    let delta = read_signed_varint(&mut index, table).unwrap_or(0);
                    read_varint(&mut index, table);
                    read_varint(&mut index, table);
                    read_varint(&mut index, table);
                    delta
                }
                13 => read_signed_varint(&mut index, table).unwrap_or(0),
                10..=12 => {
                    index += 2;
                    (code - 10).into()
                }
                _ => {
                    index += 1;
                    0
                }
            };
            line_number += line_delta as i32;
            if bytecode_address >= lasti {
                break;
            }
        }
        line_number
    }
}

impl Object for PyObject {
    type TypeObject = PyTypeObject;

    fn ob_type(&self) -> *mut Self::TypeObject {
        self.ob_type
    }
}

impl TypeObject for PyTypeObject {
    fn name(&self) -> *const c_char {
        self.tp_name
    }

    fn dictoffset(&self) -> isize {
        self.tp_dictoffset
    }

    fn flags(&self) -> usize {
        self.tp_flags as usize
    }
}

impl BytesObject for PyBytesObject {
    fn size(&self) -> usize {
        self.ob_size as usize
    }

    fn address(&self, base: usize) -> usize {
        base + 48
    }
}

impl StringObject for PyUnicodeObject {
    fn ascii(&self) -> bool {
        (self.state >> 4) & 0x01 != 0
    }

    fn kind(&self) -> u32 {
        (self.state & 0x07) as u32
    }

    fn size(&self) -> usize {
        self.length as usize
    }

    fn address(&self, base: usize) -> usize {
        const COMPACT_MASK: u8 = 0x08;
        if self.state & COMPACT_MASK == 0 {
            self.data as usize
        } else if self.ascii() {
            base + 56
        } else {
            base + 72
        }
    }
}

impl TupleObject for PyTupleObject {
    fn size(&self) -> usize {
        self.ob_size as usize
    }

    fn address(&self, base: usize, index: usize) -> usize {
        base + 48 + index * std::mem::size_of::<*mut PyObject>()
    }
}

impl ListObject for PyListObject {
    type Object = PyObject;

    fn size(&self) -> usize {
        self.ob_size as usize
    }

    fn item(&self) -> *mut *mut Self::Object {
        self.ob_item
    }
}

fn read_varint(index: &mut usize, table: &[u8]) -> Option<usize> {
    if *index >= table.len() {
        return None;
    }
    let mut byte = table[*index];
    let mut ret = (byte & 63) as usize;
    let mut shift = 0;
    *index += 1;

    while byte & 64 != 0 {
        if *index >= table.len() {
            return None;
        }
        byte = table[*index];
        *index += 1;
        shift += 6;
        ret += ((byte & 63) as usize) << shift;
    }
    Some(ret)
}

fn read_signed_varint(index: &mut usize, table: &[u8]) -> Option<isize> {
    let unsigned_val = read_varint(index, table)?;
    if unsigned_val & 1 != 0 {
        Some(-((unsigned_val >> 1) as isize))
    } else {
        Some((unsigned_val >> 1) as isize)
    }
}
