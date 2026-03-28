use anyhow::Error;
use remoteprocess::ProcessMemory;

use crate::python_bindings::v3_13_0;
use crate::version::Version;

/// Version-agnostic struct holding all debug offsets py-spy needs.
/// Populated from `_Py_DebugOffsets` read from the target process.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct DebugOffsets {
    pub version: u64,
    pub free_threaded: bool,

    // Runtime state
    pub runtime_interpreters_head: u64,

    // Interpreter state
    pub interp_size: u64,
    pub interp_threads_head: u64,
    pub interp_imports_modules: u64,
    pub interp_gil_runtime_state: u64,
    pub interp_gil_runtime_state_enabled: u64,
    pub interp_gil_runtime_state_locked: u64,
    pub interp_gil_runtime_state_holder: u64,

    // Thread state
    pub thread_size: u64,
    pub thread_prev: u64,
    pub thread_next: u64,
    pub thread_interp: u64,
    pub thread_current_frame: u64,
    pub thread_id: u64,
    pub thread_native_thread_id: u64,

    // Interpreter frame
    pub frame_size: u64,
    pub frame_previous: u64,
    pub frame_executable: u64,
    pub frame_instr_ptr: u64,
    pub frame_localsplus: u64,
    pub frame_owner: u64,
    pub frame_tlbc_index: u64,

    // Code object
    pub code_size: u64,
    pub code_filename: u64,
    pub code_name: u64,
    pub code_qualname: u64,
    pub code_linetable: u64,
    pub code_firstlineno: u64,
    pub code_argcount: u64,
    pub code_localsplusnames: u64,
    pub code_localspluskinds: u64,
    pub code_co_code_adaptive: u64,
    pub code_co_tlbc: u64,

    // PyObject
    pub pyobject_size: u64,
    pub pyobject_ob_type: u64,

    // Type object
    pub type_object_tp_name: u64,
    pub type_object_tp_flags: u64,

    // Tuple object
    pub tuple_ob_item: u64,
    pub tuple_ob_size: u64,

    // List object
    pub list_ob_item: u64,
    pub list_ob_size: u64,

    // Dict object
    pub dict_ma_keys: u64,
    pub dict_ma_values: u64,

    // Float object
    pub float_ob_fval: u64,

    // Long object
    pub long_lv_tag: u64,
    pub long_ob_digit: u64,

    // Bytes object
    pub bytes_size: u64,
    pub bytes_ob_size: u64,
    pub bytes_ob_sval: u64,

    // Unicode object
    pub unicode_size: u64,
    pub unicode_state: u64,
    pub unicode_length: u64,
    pub unicode_asciiobject_size: u64,
}

// ---------------------------------------------------------------------------
// Hand-written #[repr(C)] struct matching CPython 3.14's _Py_DebugOffsets
// layout. All fields are u64 (matching the C definition). This struct differs
// from 3.13 in:
//   - interpreter_state: threads_main inserted after threads_head;
//     code_object_generation + tlbc_generation appended.
//   - interpreter_frame: stackpointer + tlbc_index appended.
//   - code_object: co_tlbc appended.
//   - set_object inserted between list_object and dict_object.
//   - gen_object, llist_node, debugger_support appended after gc.
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314Raw {
    pub cookie: [u8; 8],
    pub version: u64,
    pub free_threaded: u64,

    pub runtime_state: DebugOffsets314RuntimeState,
    pub interpreter_state: DebugOffsets314InterpState,
    pub thread_state: DebugOffsets314ThreadState,
    pub interpreter_frame: DebugOffsets314Frame,
    pub code_object: DebugOffsets314CodeObject,
    pub pyobject: DebugOffsets314PyObject,
    pub type_object: DebugOffsets314TypeObject,
    pub tuple_object: DebugOffsets314TupleObject,
    pub list_object: DebugOffsets314ListObject,
    pub set_object: DebugOffsets314SetObject,
    pub dict_object: DebugOffsets314DictObject,
    pub float_object: DebugOffsets314FloatObject,
    pub long_object: DebugOffsets314LongObject,
    pub bytes_object: DebugOffsets314BytesObject,
    pub unicode_object: DebugOffsets314UnicodeObject,
    pub gc: DebugOffsets314Gc,
    pub gen_object: DebugOffsets314GenObject,
    pub llist_node: DebugOffsets314LlistNode,
    pub debugger_support: DebugOffsets314DebuggerSupport,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314RuntimeState {
    pub size: u64,
    pub finalizing: u64,
    pub interpreters_head: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314InterpState {
    pub size: u64,
    pub id: u64,
    pub next: u64,
    pub threads_head: u64,
    pub threads_main: u64,
    pub gc: u64,
    pub imports_modules: u64,
    pub sysdict: u64,
    pub builtins: u64,
    pub ceval_gil: u64,
    pub gil_runtime_state: u64,
    pub gil_runtime_state_enabled: u64,
    pub gil_runtime_state_locked: u64,
    pub gil_runtime_state_holder: u64,
    pub code_object_generation: u64,
    pub tlbc_generation: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314ThreadState {
    pub size: u64,
    pub prev: u64,
    pub next: u64,
    pub interp: u64,
    pub current_frame: u64,
    pub thread_id: u64,
    pub native_thread_id: u64,
    pub datastack_chunk: u64,
    pub status: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314Frame {
    pub size: u64,
    pub previous: u64,
    pub executable: u64,
    pub instr_ptr: u64,
    pub localsplus: u64,
    pub owner: u64,
    pub stackpointer: u64,
    pub tlbc_index: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314CodeObject {
    pub size: u64,
    pub filename: u64,
    pub name: u64,
    pub qualname: u64,
    pub linetable: u64,
    pub firstlineno: u64,
    pub argcount: u64,
    pub localsplusnames: u64,
    pub localspluskinds: u64,
    pub co_code_adaptive: u64,
    pub co_tlbc: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314PyObject {
    pub size: u64,
    pub ob_type: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314TypeObject {
    pub size: u64,
    pub tp_name: u64,
    pub tp_repr: u64,
    pub tp_flags: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314TupleObject {
    pub size: u64,
    pub ob_item: u64,
    pub ob_size: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314ListObject {
    pub size: u64,
    pub ob_item: u64,
    pub ob_size: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314SetObject {
    pub size: u64,
    pub used: u64,
    pub table: u64,
    pub mask: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314DictObject {
    pub size: u64,
    pub ma_keys: u64,
    pub ma_values: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314FloatObject {
    pub size: u64,
    pub ob_fval: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314LongObject {
    pub size: u64,
    pub lv_tag: u64,
    pub ob_digit: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314BytesObject {
    pub size: u64,
    pub ob_size: u64,
    pub ob_sval: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314UnicodeObject {
    pub size: u64,
    pub state: u64,
    pub length: u64,
    pub asciiobject_size: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314Gc {
    pub size: u64,
    pub collecting: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314GenObject {
    pub size: u64,
    pub gi_name: u64,
    pub gi_iframe: u64,
    pub gi_frame_state: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314LlistNode {
    pub next: u64,
    pub prev: u64,
}

#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct DebugOffsets314DebuggerSupport {
    pub eval_breaker: u64,
    pub remote_debugger_support: u64,
    pub remote_debugging_enabled: u64,
    pub debugger_pending_call: u64,
    pub debugger_script_path: u64,
    pub debugger_script_path_size: u64,
}


/// Extract shared fields from a raw debug offsets struct into DebugOffsets.
/// The raw types (3.13 bindgen and 3.14 hand-written) have identical field names
/// for the subset DebugOffsets uses.
macro_rules! debug_offsets_from_raw {
    ($raw:expr, $frame_tlbc_index:expr, $code_co_tlbc:expr) => {
        DebugOffsets {
            version: $raw.version,
            free_threaded: $raw.free_threaded != 0,

            runtime_interpreters_head: $raw.runtime_state.interpreters_head,

            interp_size: $raw.interpreter_state.size,
            interp_threads_head: $raw.interpreter_state.threads_head,
            interp_imports_modules: $raw.interpreter_state.imports_modules,
            interp_gil_runtime_state: $raw.interpreter_state.gil_runtime_state,
            // Absolute offset from interpreter base: base of _gil + offset of enabled within it
            interp_gil_runtime_state_enabled: $raw.interpreter_state.gil_runtime_state
                + $raw.interpreter_state.gil_runtime_state_enabled,
            interp_gil_runtime_state_locked: $raw.interpreter_state.gil_runtime_state_locked,
            interp_gil_runtime_state_holder: $raw.interpreter_state.gil_runtime_state_holder,

            thread_size: $raw.thread_state.size,
            thread_prev: $raw.thread_state.prev,
            thread_next: $raw.thread_state.next,
            thread_interp: $raw.thread_state.interp,
            thread_current_frame: $raw.thread_state.current_frame,
            thread_id: $raw.thread_state.thread_id,
            thread_native_thread_id: $raw.thread_state.native_thread_id,

            frame_size: $raw.interpreter_frame.size,
            frame_previous: $raw.interpreter_frame.previous,
            frame_executable: $raw.interpreter_frame.executable,
            frame_instr_ptr: $raw.interpreter_frame.instr_ptr,
            frame_localsplus: $raw.interpreter_frame.localsplus,
            frame_owner: $raw.interpreter_frame.owner,
            frame_tlbc_index: $frame_tlbc_index,

            code_size: $raw.code_object.size,
            code_filename: $raw.code_object.filename,
            code_name: $raw.code_object.name,
            code_qualname: $raw.code_object.qualname,
            code_linetable: $raw.code_object.linetable,
            code_firstlineno: $raw.code_object.firstlineno,
            code_argcount: $raw.code_object.argcount,
            code_localsplusnames: $raw.code_object.localsplusnames,
            code_localspluskinds: $raw.code_object.localspluskinds,
            code_co_code_adaptive: $raw.code_object.co_code_adaptive,
            code_co_tlbc: $code_co_tlbc,

            pyobject_size: $raw.pyobject.size,
            pyobject_ob_type: $raw.pyobject.ob_type,

            type_object_tp_name: $raw.type_object.tp_name,
            type_object_tp_flags: $raw.type_object.tp_flags,

            tuple_ob_item: $raw.tuple_object.ob_item,
            tuple_ob_size: $raw.tuple_object.ob_size,

            list_ob_item: $raw.list_object.ob_item,
            list_ob_size: $raw.list_object.ob_size,

            dict_ma_keys: $raw.dict_object.ma_keys,
            dict_ma_values: $raw.dict_object.ma_values,

            float_ob_fval: $raw.float_object.ob_fval,

            long_lv_tag: $raw.long_object.lv_tag,
            long_ob_digit: $raw.long_object.ob_digit,

            bytes_size: $raw.bytes_object.size,
            bytes_ob_size: $raw.bytes_object.ob_size,
            bytes_ob_sval: $raw.bytes_object.ob_sval,

            unicode_size: $raw.unicode_object.size,
            unicode_state: $raw.unicode_object.state,
            unicode_length: $raw.unicode_object.length,
            unicode_asciiobject_size: $raw.unicode_object.asciiobject_size,
        }
    };
}

impl DebugOffsets {
    pub fn from_3_13(raw: &v3_13_0::_Py_DebugOffsets) -> Self {
        debug_offsets_from_raw!(raw, 0, 0)
    }

    pub fn from_3_14(raw: &DebugOffsets314Raw) -> Self {
        debug_offsets_from_raw!(raw, raw.interpreter_frame.tlbc_index, raw.code_object.co_tlbc)
    }

    /// Read `_Py_DebugOffsets` from a target process at the given `_PyRuntime` address.
    pub fn read<P: ProcessMemory>(
        process: &P,
        runtime_addr: usize,
        version: &Version,
    ) -> Result<Self, Error> {
        match version {
            Version {
                major: 3,
                minor: 13,
                ..
            } => {
                let raw: v3_13_0::_Py_DebugOffsets = process.copy_struct(runtime_addr)?;
                let cookie: &[u8; 8] =
                    unsafe { &*(&raw.cookie as *const [std::os::raw::c_char; 8] as *const [u8; 8]) };
                validate_cookie(cookie)?;
                Ok(Self::from_3_13(&raw))
            }
            Version {
                major: 3,
                minor: 14,
                ..
            } => {
                let raw: DebugOffsets314Raw = process.copy_struct(runtime_addr)?;
                validate_cookie(&raw.cookie)?;
                Ok(Self::from_3_14(&raw))
            }
            _ => Err(format_err!(
                "DebugOffsets not supported for Python {}.{}",
                version.major,
                version.minor
            )),
        }
    }
}

fn validate_cookie(cookie: &[u8; 8]) -> Result<(), Error> {
    if cookie == b"xdebugpy" {
        Ok(())
    } else {
        Err(format_err!(
            "Invalid _Py_DebugOffsets cookie: expected 'xdebugpy', got {:?}",
            cookie
        ))
    }
}

// ---------------------------------------------------------------------------
// Read primitives: extract fields from remote process memory at given offsets
// ---------------------------------------------------------------------------

/// Read a pointer-sized value from `base + offset` in the target process.
pub fn read_ptr<P: ProcessMemory>(process: &P, base: usize, offset: u64) -> Result<usize, Error> {
    let mut buf = [0u8; std::mem::size_of::<usize>()];
    process.read(base + offset as usize, &mut buf)?;
    Ok(usize::from_ne_bytes(buf))
}

/// Read a 4-byte signed integer from `base + offset`.
pub fn read_i32<P: ProcessMemory>(process: &P, base: usize, offset: u64) -> Result<i32, Error> {
    let mut buf = [0u8; 4];
    process.read(base + offset as usize, &mut buf)?;
    Ok(i32::from_ne_bytes(buf))
}

/// Read a 4-byte unsigned integer from `base + offset`.
pub fn read_u32<P: ProcessMemory>(process: &P, base: usize, offset: u64) -> Result<u32, Error> {
    let mut buf = [0u8; 4];
    process.read(base + offset as usize, &mut buf)?;
    Ok(u32::from_ne_bytes(buf))
}

/// Read an 8-byte unsigned integer from `base + offset`.
#[allow(dead_code)]
pub fn read_u64<P: ProcessMemory>(process: &P, base: usize, offset: u64) -> Result<u64, Error> {
    let mut buf = [0u8; 8];
    process.read(base + offset as usize, &mut buf)?;
    Ok(u64::from_ne_bytes(buf))
}

/// Read a single byte (as i8) from `base + offset`.
#[allow(dead_code)]
pub fn read_byte<P: ProcessMemory>(process: &P, base: usize, offset: u64) -> Result<i8, Error> {
    let mut buf = [0u8; 1];
    process.read(base + offset as usize, &mut buf)?;
    Ok(buf[0] as i8)
}

// ---------------------------------------------------------------------------
// Bulk struct reading: read an entire struct in one syscall, extract fields locally
// ---------------------------------------------------------------------------

/// A buffer holding a struct's raw bytes read from a remote process.
/// Fields are extracted locally without additional syscalls.
pub struct StructBuf {
    buf: Vec<u8>,
}

impl StructBuf {
    /// Read `size` bytes from `addr` in the target process.
    pub fn read<P: ProcessMemory>(process: &P, addr: usize, size: u64) -> Result<Self, Error> {
        let buf = process.copy(addr, size as usize)?;
        Ok(StructBuf { buf })
    }

    pub fn ptr_at(&self, offset: u64) -> usize {
        let off = offset as usize;
        let sz = std::mem::size_of::<usize>();
        let bytes = &self.buf[off..off + sz];
        usize::from_ne_bytes(bytes.try_into().unwrap())
    }

    pub fn i32_at(&self, offset: u64) -> i32 {
        let off = offset as usize;
        let bytes = &self.buf[off..off + 4];
        i32::from_ne_bytes(bytes.try_into().unwrap())
    }

    #[allow(dead_code)]
    pub fn u32_at(&self, offset: u64) -> u32 {
        let off = offset as usize;
        let bytes = &self.buf[off..off + 4];
        u32::from_ne_bytes(bytes.try_into().unwrap())
    }

    pub fn byte_at(&self, offset: u64) -> i8 {
        self.buf[offset as usize] as i8
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_cookie() {
        assert!(validate_cookie(b"xdebugpy").is_ok());
        assert!(validate_cookie(b"notvalid").is_err());
        assert!(validate_cookie(b"\0\0\0\0\0\0\0\0").is_err());
    }

    #[test]
    fn test_debug_offsets_314_raw_size() {
        // Must match the CPython 3.14 _Py_DebugOffsets struct layout:
        // cookie(8) + version(8) + free_threaded(8) + runtime_state(3*8) +
        // interpreter_state(16*8) + thread_state(9*8) + interpreter_frame(8*8) +
        // code_object(11*8) + pyobject(2*8) + type_object(4*8) + tuple_object(3*8) +
        // list_object(3*8) + set_object(4*8) + dict_object(3*8) + float_object(2*8) +
        // long_object(3*8) + bytes_object(3*8) + unicode_object(4*8) + gc(2*8) +
        // gen_object(4*8) + llist_node(2*8) + debugger_support(6*8) = 760
        assert_eq!(std::mem::size_of::<DebugOffsets314Raw>(), 760);
    }

    #[test]
    fn test_debug_offsets_313_oracle() {
        // Validate that from_3_13 produces correct values by comparing against
        // std::mem::offset_of! on the bindgen structs.
        use crate::python_bindings::v3_13_0::*;

        let mut raw: _Py_DebugOffsets = Default::default();
        // Populate with the same values CPython would set
        raw.runtime_state.interpreters_head =
            std::mem::offset_of!(pyruntimestate, interpreters.head) as u64;
        raw.interpreter_state.threads_head =
            std::mem::offset_of!(PyInterpreterState, threads.head) as u64;
        raw.interpreter_state.imports_modules =
            std::mem::offset_of!(PyInterpreterState, imports.modules) as u64;
        raw.interpreter_frame.previous =
            std::mem::offset_of!(_PyInterpreterFrame, previous) as u64;
        raw.interpreter_frame.executable =
            std::mem::offset_of!(_PyInterpreterFrame, f_executable) as u64;
        raw.interpreter_frame.instr_ptr =
            std::mem::offset_of!(_PyInterpreterFrame, instr_ptr) as u64;
        raw.interpreter_frame.owner = std::mem::offset_of!(_PyInterpreterFrame, owner) as u64;
        raw.code_object.filename = std::mem::offset_of!(PyCodeObject, co_filename) as u64;
        raw.code_object.name = std::mem::offset_of!(PyCodeObject, co_name) as u64;
        raw.code_object.firstlineno = std::mem::offset_of!(PyCodeObject, co_firstlineno) as u64;
        raw.thread_state.current_frame =
            std::mem::offset_of!(PyThreadState, current_frame) as u64;
        raw.thread_state.thread_id = std::mem::offset_of!(PyThreadState, thread_id) as u64;
        raw.thread_state.native_thread_id =
            std::mem::offset_of!(PyThreadState, native_thread_id) as u64;
        raw.thread_state.next = std::mem::offset_of!(PyThreadState, next) as u64;

        let offsets = DebugOffsets::from_3_13(&raw);

        assert_eq!(
            offsets.runtime_interpreters_head,
            std::mem::offset_of!(pyruntimestate, interpreters.head) as u64
        );
        assert_eq!(
            offsets.interp_threads_head,
            std::mem::offset_of!(PyInterpreterState, threads.head) as u64
        );
        assert_eq!(
            offsets.frame_previous,
            std::mem::offset_of!(_PyInterpreterFrame, previous) as u64
        );
        assert_eq!(
            offsets.frame_executable,
            std::mem::offset_of!(_PyInterpreterFrame, f_executable) as u64
        );
        assert_eq!(
            offsets.code_filename,
            std::mem::offset_of!(PyCodeObject, co_filename) as u64
        );
        assert_eq!(
            offsets.thread_current_frame,
            std::mem::offset_of!(PyThreadState, current_frame) as u64
        );
    }

    #[test]
    fn test_read_primitives_local() {
        use remoteprocess::LocalProcess;

        let local = LocalProcess;
        let val: usize = 0xDEAD_BEEF_CAFE_BABE;
        let addr = &val as *const usize as usize;

        let result = read_ptr(&local, addr, 0).unwrap();
        assert_eq!(result, val);

        let val32: i32 = -42;
        let addr32 = &val32 as *const i32 as usize;
        let result32 = read_i32(&local, addr32, 0).unwrap();
        assert_eq!(result32, -42);

        let byte_val: i8 = -1;
        let addr_byte = &byte_val as *const i8 as usize;
        let result_byte = read_byte(&local, addr_byte, 0).unwrap();
        assert_eq!(result_byte, -1);
    }
}
