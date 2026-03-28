use anyhow::{Context, Error};
use remoteprocess::ProcessMemory;

use crate::config::LineNo;
use crate::debug_offsets::{read_i32, read_ptr, read_u32, DebugOffsets, StructBuf};
use crate::python_interpreters::get_line_number_compact;
use crate::stack_trace::{Frame, LocalVariable, StackTrace};
use crate::version::Version;

// Flags from CPython's Include/internal/pycore_code.h
const CO_FAST_LOCAL: u8 = 0x20;
const CO_FAST_ARG: u8 = 0x0E; // CO_FAST_ARG_POS | CO_FAST_ARG_KW | CO_FAST_ARG_VAR

/// FRAME_OWNED_BY_CSTACK changed value between 3.13 and 3.14:
/// 3.13: FRAME_OWNED_BY_CSTACK = 3
/// 3.14: FRAME_OWNED_BY_CSTACK = 4 (new FRAME_OWNED_BY_INTERPRETER = 3 inserted)
fn frame_owned_by_cstack(version: &Version) -> i8 {
    if version.minor >= 14 {
        4
    } else {
        3
    }
}

/// Extract a PyObject pointer from a _PyStackRef value (3.14+, GIL-enabled builds).
/// In GIL-enabled builds: Py_TAG_REFCNT = 1, null = bits == 1, pointer = bits & !1.
fn stackref_to_ptr(bits: usize) -> Option<usize> {
    if bits == 1 {
        return None;
    } // PyStackRef_NULL
    if bits & 3 == 3 {
        return None;
    } // tagged int
    Some(bits & !1usize)
}

/// Read a unicode string from a PyUnicodeObject at the given address, using debug offsets.
pub fn read_unicode_string<P: ProcessMemory>(
    process: &P,
    addr: usize,
    offsets: &DebugOffsets,
) -> Result<String, Error> {
    if addr == 0 {
        return Err(format_err!("null unicode object"));
    }

    // The state struct layout differs between GIL-enabled and free-threaded builds
    // (see cpython/Include/cpython/unicodeobject.h).
    // In GIL-enabled: interned is a 2-bit bitfield packed with the others into one u32:
    //   interned:2 | kind:3 | compact:1 | ascii:1 | statically_allocated:1 | :24
    // In free-threaded: interned is a full unsigned char (for atomic access), followed
    // by the remaining bitfields in a separate unsigned int:
    //   byte 0: interned (unsigned char)
    //   byte 1+: kind:3 | compact:1 | ascii:1 | statically_allocated:1
    let (kind, compact, ascii) = if offsets.free_threaded {
        let mut buf = [0u8; 2];
        process.read(addr + offsets.unicode_state as usize, &mut buf)?;
        let kind_byte = buf[1];
        let kind = (kind_byte & 7) as u32;
        let compact = ((kind_byte >> 3) & 1) as u32;
        let ascii = ((kind_byte >> 4) & 1) as u32;
        (kind, compact, ascii)
    } else {
        let state = read_u32(process, addr, offsets.unicode_state)?;
        let kind = (state >> 2) & 7;
        let compact = (state >> 5) & 1;
        let ascii = (state >> 6) & 1;
        (kind, compact, ascii)
    };

    let length = read_ptr(process, addr, offsets.unicode_length)? as usize;

    if length == 0 {
        return Ok(String::new());
    }
    if length >= 4096 {
        return Err(format_err!("Refusing to copy {} chars of a string", length));
    }

    let data_addr = if compact != 0 {
        if ascii != 0 {
            // Compact ASCII: data follows PyASCIIObject
            addr + offsets.unicode_asciiobject_size as usize
        } else {
            // Compact non-ASCII: data follows PyCompactUnicodeObject.
            // PyCompactUnicodeObject = PyASCIIObject + utf8_length(Py_ssize_t) + utf8(char*)
            let compact_unicode_size =
                offsets.unicode_asciiobject_size as usize + 2 * std::mem::size_of::<usize>();
            addr + compact_unicode_size
        }
    } else {
        // Non-compact: data pointer is stored inline. This is rare in practice.
        // The data.any pointer lives right after the PyCompactUnicodeObject.
        // PyCompactUnicodeObject size == offsets.unicode_size, and the data pointer
        // is at that offset.
        let data_ptr: usize = process.copy_struct(addr + offsets.unicode_size as usize)?;
        data_ptr
    };

    let byte_len = length * kind as usize;
    let bytes = process.copy(data_addr, byte_len)?;
    crate::python_data_access::decode_unicode_bytes(kind, ascii != 0, &bytes)
}

/// Read a bytes object (e.g. co_linetable) from a PyBytesObject at the given address.
fn read_bytes_object<P: ProcessMemory>(
    process: &P,
    addr: usize,
    offsets: &DebugOffsets,
) -> Result<Vec<u8>, Error> {
    if addr == 0 {
        return Err(format_err!("null bytes object"));
    }

    // ob_size is Py_ssize_t, which is pointer-sized
    let size = read_ptr(process, addr, offsets.bytes_ob_size)? as usize;
    if size >= 65536 {
        return Err(format_err!("Refusing to copy {} bytes", size));
    }
    Ok(process.copy(addr + offsets.bytes_ob_sval as usize, size)?)
}


// ---------------------------------------------------------------------------
// GIL detection
// ---------------------------------------------------------------------------

fn get_gil_thread_id<P: ProcessMemory>(
    process: &P,
    interpreter_address: usize,
    offsets: &DebugOffsets,
) -> Result<u64, Error> {
    // In free-threaded builds, check if the GIL is enabled at all
    if offsets.free_threaded {
        let enabled =
            read_i32(process, interpreter_address, offsets.interp_gil_runtime_state_enabled)?;
        if enabled == 0 {
            return Ok(0);
        }
    }

    let locked = read_i32(process, interpreter_address, offsets.interp_gil_runtime_state_locked)?;
    if locked == 0 {
        return Ok(0);
    }

    let holder_addr = read_ptr(
        process,
        interpreter_address,
        offsets.interp_gil_runtime_state_holder,
    )?;
    if holder_addr == 0 {
        return Ok(0);
    }

    // Read thread_id from the holder's PyThreadState
    let tid = read_ptr(process, holder_addr, offsets.thread_id)?;
    Ok(tid as u64)
}

// ---------------------------------------------------------------------------
// Stack trace collection
// ---------------------------------------------------------------------------

pub fn get_stack_traces_via_offsets<P: ProcessMemory>(
    interpreter_address: usize,
    process: &P,
    offsets: &DebugOffsets,
    version: &Version,
    lineno: LineNo,
    copy_locals: bool,
) -> Result<Vec<StackTrace>, Error> {
    let gil_thread_id = get_gil_thread_id(process, interpreter_address, offsets)?;

    // Read head of thread linked list
    let mut thread_addr: usize =
        read_ptr(process, interpreter_address, offsets.interp_threads_head)
            .context("Failed to read threads.head")?;

    let mut ret = Vec::new();
    let cstack_owner = frame_owned_by_cstack(version);

    while thread_addr != 0 {
        let ts = StructBuf::read(process, thread_addr, offsets.thread_size)?;
        let thread_id = ts.ptr_at(offsets.thread_id) as u64;
        let native_thread_id = ts.ptr_at(offsets.thread_native_thread_id);
        let current_frame_addr = ts.ptr_at(offsets.thread_current_frame);

        let frames = get_frame_stack(
            process,
            current_frame_addr,
            offsets,
            version,
            lineno,
            cstack_owner,
            copy_locals,
        )?;

        let mut trace = StackTrace {
            pid: 0,
            thread_id,
            thread_name: None,
            os_thread_id: Some(native_thread_id as u64),
            active: true,
            owns_gil: thread_id == gil_thread_id,
            frames,
            process_info: None,
        };

        if let Some(last) = trace.frames.last_mut() {
            last.is_shim_entry = true;
        }

        ret.push(trace);
        if ret.len() > 4096 {
            return Err(format_err!("Max thread recursion depth reached"));
        }

        thread_addr = ts.ptr_at(offsets.thread_next);
    }

    Ok(ret)
}

fn get_frame_stack<P: ProcessMemory>(
    process: &P,
    mut frame_addr: usize,
    offsets: &DebugOffsets,
    version: &Version,
    lineno: LineNo,
    cstack_owner: i8,
    copy_locals: bool,
) -> Result<Vec<Frame>, Error> {
    let mut frames = Vec::new();

    let set_last_as_shim = |frames: &mut Vec<Frame>| {
        if let Some(f) = frames.last_mut() {
            f.is_shim_entry = true;
        }
    };

    while frame_addr != 0 {
        let frame = StructBuf::read(process, frame_addr, offsets.frame_size)?;
        let raw_executable = frame.ptr_at(offsets.frame_executable);
        let code_addr = if version.minor >= 14 {
            match stackref_to_ptr(raw_executable) {
                Some(a) => a,
                None => {
                    frame_addr = frame.ptr_at(offsets.frame_previous);
                    set_last_as_shim(&mut frames);
                    continue;
                }
            }
        } else if raw_executable == 0 {
            frame_addr = frame.ptr_at(offsets.frame_previous);
            set_last_as_shim(&mut frames);
            continue;
        } else {
            raw_executable
        };

        let code = match StructBuf::read(process, code_addr, offsets.code_size) {
            Ok(c) => c,
            Err(_) => {
                frame_addr = frame.ptr_at(offsets.frame_previous);
                set_last_as_shim(&mut frames);
                continue;
            }
        };

        let filename_ptr = code.ptr_at(offsets.code_filename);
        let name_ptr = code.ptr_at(offsets.code_name);
        let filename = match read_unicode_string(process, filename_ptr, offsets) {
            Ok(s) => s,
            Err(_) => {
                frame_addr = frame.ptr_at(offsets.frame_previous);
                set_last_as_shim(&mut frames);
                continue;
            }
        };
        let name = match read_unicode_string(process, name_ptr, offsets) {
            Ok(s) => s,
            Err(_) => {
                frame_addr = frame.ptr_at(offsets.frame_previous);
                set_last_as_shim(&mut frames);
                continue;
            }
        };

        if filename.is_empty() || filename == "<shim>" {
            frame_addr = frame.ptr_at(offsets.frame_previous);
            set_last_as_shim(&mut frames);
            continue;
        }

        let line = match lineno {
            LineNo::NoLine => 0,
            LineNo::First => code.i32_at(offsets.code_firstlineno),
            LineNo::LastInstruction => {
                let instr_ptr = frame.ptr_at(offsets.frame_instr_ptr);
                match compute_line_number_from_bufs(
                    process, &frame, instr_ptr, code_addr, &code, offsets,
                ) {
                    Ok(l) => l,
                    Err(e) => {
                        warn!("Failed to get line number from {}.{}: {}", filename, name, e);
                        0
                    }
                }
            }
        };

        let owner = frame.byte_at(offsets.frame_owner);
        let is_entry = owner == cstack_owner;

        let locals = if copy_locals {
            match get_locals_via_offsets(process, frame_addr, &code, offsets, version) {
                Ok(l) => Some(l),
                Err(e) => {
                    warn!("Failed to get locals for {}.{}: {}", filename, name, e);
                    None
                }
            }
        } else {
            None
        };

        frames.push(Frame {
            name,
            filename,
            line,
            short_filename: None,
            module: None,
            locals,
            is_entry,
            is_shim_entry: false,
        });

        if frames.len() > 4096 {
            return Err(format_err!("Max frame recursion depth reached"));
        }

        frame_addr = frame.ptr_at(offsets.frame_previous);
    }

    Ok(frames)
}

/// Compute line number using pre-read frame instr_ptr and code StructBuf.
/// Handles TLBC (thread-local bytecode) in free-threaded builds.
fn compute_line_number_from_bufs<P: ProcessMemory>(
    process: &P,
    frame: &StructBuf,
    instr_ptr: usize,
    code_addr: usize,
    code: &StructBuf,
    offsets: &DebugOffsets,
) -> Result<i32, Error> {
    let default_lasti =
        (instr_ptr as i64 - code_addr as i64 - offsets.code_co_code_adaptive as i64) as i32;
    let lasti = if offsets.free_threaded && offsets.frame_tlbc_index != 0 {
        let tlbc_index = frame.i32_at(offsets.frame_tlbc_index);
        if tlbc_index > 0 && offsets.code_co_tlbc != 0 {
            // instr_ptr is in a thread-local bytecode copy, not in co_code_adaptive.
            let co_tlbc_ptr = code.ptr_at(offsets.code_co_tlbc);
            if co_tlbc_ptr != 0 {
                // _PyCodeArray: { Py_ssize_t size, int capacity, char *entries[1] }
                let entries_offset = 16usize; // 8 + 4 + 4 padding on 64-bit
                let ptr_size = std::mem::size_of::<usize>();
                let entry_addr: usize = process.copy_struct(
                    co_tlbc_ptr + entries_offset + tlbc_index as usize * ptr_size,
                )?;
                if entry_addr != 0 {
                    (instr_ptr as i64 - entry_addr as i64) as i32
                } else {
                    default_lasti
                }
            } else {
                default_lasti
            }
        } else {
            default_lasti
        }
    } else {
        default_lasti
    };

    let firstlineno = code.i32_at(offsets.code_firstlineno);

    let linetable_ptr = code.ptr_at(offsets.code_linetable);
    let table = read_bytes_object(process, linetable_ptr, offsets)
        .context("Failed to copy line number table")?;

    Ok(get_line_number_compact(firstlineno, lasti, &table))
}

// ---------------------------------------------------------------------------
// Local variable reading
// ---------------------------------------------------------------------------

/// Read local variables from a frame using debug offsets.
/// Derives nlocals from co_localspluskinds rather than reading co_nlocals
/// (which isn't exposed in _Py_DebugOffsets).
fn get_locals_via_offsets<P: ProcessMemory>(
    process: &P,
    frame_addr: usize,
    code: &StructBuf,
    offsets: &DebugOffsets,
    version: &Version,
) -> Result<Vec<LocalVariable>, Error> {
    let argcount = code.i32_at(offsets.code_argcount) as usize;

    let kinds_ptr = code.ptr_at(offsets.code_localspluskinds);
    let kinds = read_bytes_object(process, kinds_ptr, offsets)?;

    let nlocals = kinds
        .iter()
        .filter(|&&k| k & CO_FAST_LOCAL != 0 || k & CO_FAST_ARG != 0)
        .count();

    if nlocals == 0 {
        return Ok(Vec::new());
    }

    let names_tuple_ptr = code.ptr_at(offsets.code_localsplusnames);
    let names_count = read_ptr(process, names_tuple_ptr, offsets.tuple_ob_size)? as usize;
    let names_items_addr = names_tuple_ptr + offsets.tuple_ob_item as usize;

    let localsplus_addr = frame_addr + offsets.frame_localsplus as usize;
    let ptr_size = std::mem::size_of::<usize>();

    let mut ret = Vec::new();
    for i in 0..nlocals.min(names_count) {
        // Read the variable name from the tuple
        let name_obj_ptr: usize = process.copy_struct(names_items_addr + i * ptr_size)?;
        let name = match read_unicode_string(process, name_obj_ptr, offsets) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Read the variable's value pointer from localsplus
        let raw_addr: usize = process.copy_struct(localsplus_addr + i * ptr_size)?;

        // On 3.14+, localsplus contains _PyStackRef values that need decoding
        let addr = if version.minor >= 14 {
            match stackref_to_ptr(raw_addr) {
                Some(a) => a,
                None => continue, // null or tagged int
            }
        } else {
            raw_addr
        };

        if addr == 0 {
            continue;
        }

        ret.push(LocalVariable {
            name,
            addr,
            arg: i < argcount,
            repr: None,
        });
    }

    Ok(ret)
}

// ---------------------------------------------------------------------------
// Dict iteration via offsets
// ---------------------------------------------------------------------------

/// Iterate a Python dict using debug offsets for the PyDictObject fields
/// and v3_12_0::PyDictKeysObject for the keys structure (which doesn't contain
/// a PyObject header and is layout-stable across GIL/free-threaded builds).
pub fn iter_dict_via_offsets<P: ProcessMemory>(
    process: &P,
    dict_addr: usize,
    offsets: &DebugOffsets,
) -> Result<Vec<(usize, usize)>, Error> {
    let ma_keys_addr = read_ptr(process, dict_addr, offsets.dict_ma_keys)?;
    let ma_values_addr = read_ptr(process, dict_addr, offsets.dict_ma_values)?;

    let keys: crate::python_bindings::v3_12_0::PyDictKeysObject =
        process.copy_struct(ma_keys_addr)?;

    let entries_addr =
        ma_keys_addr + (1 << keys.dk_log2_index_bytes) + std::mem::size_of_val(&keys);

    // dk_kind determines the entry format:
    //   0 = PyDictKeyEntry with hash (3 pointer-sized fields: hash, key, value)
    //   1+ = PyDictUnicodeEntry without hash (2 pointer-sized fields: key, value)
    let ptr_size = std::mem::size_of::<usize>();
    let entry_has_hash = keys.dk_kind == 0;
    let entry_size = if entry_has_hash { 3 * ptr_size } else { 2 * ptr_size };

    let mut ret = Vec::new();
    for index in 0..keys.dk_nentries as usize {
        let addr = entries_addr + index * entry_size;
        let (key, entry_value) = if entry_has_hash {
            // { me_hash: Py_hash_t, me_key: *PyObject, me_value: *PyObject }
            let me_key: usize = process.copy_struct(addr + ptr_size)?;
            let me_value: usize = process.copy_struct(addr + 2 * ptr_size)?;
            (me_key, me_value)
        } else {
            // { me_key: *PyObject, me_value: *PyObject }
            let me_key: usize = process.copy_struct(addr)?;
            let me_value: usize = process.copy_struct(addr + ptr_size)?;
            (me_key, me_value)
        };
        if key == 0 {
            continue;
        }
        let value = if ma_values_addr != 0 {
            process.copy_struct(ma_values_addr + index * ptr_size)?
        } else {
            entry_value
        };
        ret.push((key, value));
    }
    Ok(ret)
}

// ---------------------------------------------------------------------------
// Variable formatting via offsets (replaces format_variable::<I> for 3.14+)
// ---------------------------------------------------------------------------

use crate::python_data_access::{
    PY_TPFLAGS_DICT_SUBCLASS, PY_TPFLAGS_LIST_SUBCLASS, PY_TPFLAGS_LONG_SUBCLASS,
    PY_TPFLAGS_STRING_SUBCLASS, PY_TPFLAGS_TUPLE_SUBCLASS,
};

pub fn format_variable_via_offsets<P: ProcessMemory>(
    process: &P,
    offsets: &DebugOffsets,
    addr: usize,
    max_length: isize,
) -> Result<String, Error> {
    if max_length <= 5 {
        return Ok("...".to_owned());
    }

    // Read ob_type pointer
    let type_addr = read_ptr(process, addr, offsets.pyobject_ob_type)?;
    if type_addr == 0 {
        return Ok(format!("<object at 0x{addr:x}>"));
    }

    // Read tp_name (a C string pointer)
    let tp_name_ptr = read_ptr(process, type_addr, offsets.type_object_tp_name)?;
    let max_type_len = 128;
    let name_bytes = process.copy(tp_name_ptr, max_type_len)?;
    let length = name_bytes.iter().position(|&x| x == 0).unwrap_or(max_type_len);
    let type_name = std::str::from_utf8(&name_bytes[..length])?;

    // Read tp_flags
    let flags = read_ptr(process, type_addr, offsets.type_object_tp_flags)?;

    let format_int = |value: i64| {
        if type_name == "bool" {
            (if value > 0 { "True" } else { "False" }).to_owned()
        } else {
            format!("{value}")
        }
    };

    let formatted = if flags & PY_TPFLAGS_LONG_SUBCLASS != 0 {
        let (value, overflowed) = read_long_via_offsets(process, offsets, addr)?;
        if overflowed {
            if value > 0 { "+bigint".to_owned() } else { "-bigint".to_owned() }
        } else {
            format_int(value)
        }
    } else if flags & PY_TPFLAGS_STRING_SUBCLASS != 0 {
        let value = read_unicode_string(process, addr, offsets)?
            .replace('\'', "\\\"")
            .replace('\n', "\\n");
        if let Some((offset, _)) = value.char_indices().nth((max_length - 5) as usize) {
            format!("\"{}...\"", &value[..offset])
        } else {
            format!("\"{value}\"")
        }
    } else if flags & PY_TPFLAGS_DICT_SUBCLASS != 0 {
        let mut values = Vec::new();
        let mut remaining = max_length - 2;
        for (key, value) in iter_dict_via_offsets(process, addr, offsets)? {
            let key = format_variable_via_offsets(process, offsets, key, remaining)?;
            let value = format_variable_via_offsets(process, offsets, value, remaining)?;
            remaining -= (key.len() + value.len()) as isize + 4;
            if remaining <= 5 {
                values.push("...".to_owned());
                break;
            }
            values.push(format!("{key}: {value}"));
        }
        format!("{{{}}}", values.join(", "))
    } else if flags & PY_TPFLAGS_LIST_SUBCLASS != 0 {
        let ob_item = read_ptr(process, addr, offsets.list_ob_item)?;
        let ob_size = read_ptr(process, addr, offsets.list_ob_size)? as usize;
        let ptr_size = std::mem::size_of::<usize>();
        let mut values = Vec::new();
        let mut remaining = max_length - 2;
        for i in 0..ob_size {
            let value_ptr: usize = process.copy_struct(ob_item + i * ptr_size)?;
            let value = format_variable_via_offsets(process, offsets, value_ptr, remaining)?;
            remaining -= value.len() as isize + 2;
            if remaining <= 5 { values.push("...".to_owned()); break; }
            values.push(value);
        }
        format!("[{}]", values.join(", "))
    } else if flags & PY_TPFLAGS_TUPLE_SUBCLASS != 0 {
        let ob_size = read_ptr(process, addr, offsets.tuple_ob_size)? as usize;
        let items_addr = addr + offsets.tuple_ob_item as usize;
        let ptr_size = std::mem::size_of::<usize>();
        let mut values = Vec::new();
        let mut remaining = max_length - 2;
        for i in 0..ob_size {
            let value_ptr: usize = process.copy_struct(items_addr + i * ptr_size)?;
            let value = format_variable_via_offsets(process, offsets, value_ptr, remaining)?;
            remaining -= value.len() as isize + 2;
            if remaining <= 5 { values.push("...".to_owned()); break; }
            values.push(value);
        }
        format!("({})", values.join(", "))
    } else if type_name == "float" {
        let ob_fval: f64 = process.copy_struct(addr + offsets.float_ob_fval as usize)?;
        format!("{ob_fval}")
    } else if type_name == "NoneType" {
        "None".to_owned()
    } else if type_name.starts_with("numpy.") {
        // Numpy scalars have layout: { PyObject ob_base, T obval }
        let obval_offset = offsets.pyobject_size as usize;
        match type_name {
            "numpy.bool" => format_obval::<bool>(addr, obval_offset, process)?,
            "numpy.uint8" => format_obval::<u8>(addr, obval_offset, process)?,
            "numpy.uint16" => format_obval::<u16>(addr, obval_offset, process)?,
            "numpy.uint32" => format_obval::<u32>(addr, obval_offset, process)?,
            "numpy.uint64" => format_obval::<u64>(addr, obval_offset, process)?,
            "numpy.int8" => format_obval::<i8>(addr, obval_offset, process)?,
            "numpy.int16" => format_obval::<i16>(addr, obval_offset, process)?,
            "numpy.int32" => format_obval::<i32>(addr, obval_offset, process)?,
            "numpy.int64" => format_obval::<i64>(addr, obval_offset, process)?,
            "numpy.float32" => format_obval::<f32>(addr, obval_offset, process)?,
            "numpy.float64" => format_obval::<f64>(addr, obval_offset, process)?,
            _ => format!("<{type_name} at 0x{addr:x}>"),
        }
    } else {
        format!("<{type_name} at 0x{addr:x}>")
    };

    Ok(formatted)
}

fn format_obval<T: std::fmt::Display + Copy>(
    addr: usize,
    obval_offset: usize,
    process: &impl ProcessMemory,
) -> Result<String, Error> {
    let result: T = process.copy_struct(addr + obval_offset)?;
    Ok(format!("{result}"))
}

/// Read a Python int (PyLongObject) using debug offsets.
/// Returns (value, overflowed).
pub fn read_long_via_offsets<P: ProcessMemory>(
    process: &P,
    offsets: &DebugOffsets,
    addr: usize,
) -> Result<(i64, bool), Error> {
    let lv_tag: usize = process.copy_struct(addr + offsets.long_lv_tag as usize)?;
    let size = lv_tag >> 3;
    let negative: i64 = if (lv_tag & 3) == 2 { -1 } else { 1 };
    let digits_addr = addr + offsets.long_ob_digit as usize;
    crate::python_data_access::decode_long(process, size, negative, digits_addr)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stackref_to_ptr() {
        // null
        assert_eq!(stackref_to_ptr(1), None);
        // tagged int (both low bits set)
        assert_eq!(stackref_to_ptr(0xFF03), None);
        assert_eq!(stackref_to_ptr(3), None);
        // regular pointer (tag bit 0 set = immortal)
        assert_eq!(stackref_to_ptr(0x1000_0001), Some(0x1000_0000));
        // regular pointer (no tag bits)
        assert_eq!(stackref_to_ptr(0x1000_0000), Some(0x1000_0000));
        // pointer with tag bit 0 (Py_TAG_REFCNT)
        assert_eq!(stackref_to_ptr(0xDEAD_BEE1), Some(0xDEAD_BEE0));
    }

    #[test]
    fn test_frame_owned_by_cstack_versions() {
        let v313 = Version {
            major: 3,
            minor: 13,
            patch: 0,
            release_flags: String::new(),
            build_metadata: None,
        };
        let v314 = Version {
            major: 3,
            minor: 14,
            patch: 0,
            release_flags: String::new(),
            build_metadata: None,
        };
        assert_eq!(frame_owned_by_cstack(&v313), 3);
        assert_eq!(frame_owned_by_cstack(&v314), 4);
    }

    #[test]
    fn test_get_line_number_compact_format() {
        // Same test data as python_interpreters::tests::test_py3_11_line_numbers
        let table = [
            128_u8, 0, 221, 4, 8, 132, 74, 136, 118, 209, 4, 22, 212, 4, 22, 208, 4, 22, 208, 4,
            22, 208, 4, 22,
        ];
        // In the trait-based code, lasti is adjusted by subtracting the offset of
        // co_code_adaptive from the code object. For this test we pass lasti directly
        // as if that adjustment already happened.
        let result = get_line_number_compact(4, 214, &table);
        assert_eq!(result, 5);
    }

    #[test]
    fn test_read_unicode_string_local() {
        use crate::python_data_access::tests::to_asciiobject;
        use remoteprocess::LocalProcess;

        let original = "hello_function";
        let obj = to_asciiobject(original);
        let addr = &obj as *const _ as usize;

        // Build offsets matching the v3_7_0 (and later) PyASCIIObject layout.
        // The state bitfield is at offset 16 (after ob_base: PyObject = 16 bytes on 64-bit).
        // length is at offset 24 (after state u32 + padding).
        // These values come from the actual bindgen struct layout.
        use crate::python_bindings::v3_7_0::PyASCIIObject;
        let offsets = DebugOffsets {
            unicode_state: std::mem::offset_of!(PyASCIIObject, state) as u64,
            unicode_length: std::mem::offset_of!(PyASCIIObject, length) as u64,
            unicode_asciiobject_size: std::mem::size_of::<PyASCIIObject>() as u64,
            unicode_size: 0, // not needed for compact+ascii
            ..DebugOffsets::default()
        };

        let result = read_unicode_string(&LocalProcess, addr, &offsets).unwrap();
        assert_eq!(result, original);
    }
}
