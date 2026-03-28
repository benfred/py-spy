use std::collections::HashMap;

use anyhow::{Context, Error};

use crate::python_bindings::{v3_10_0, v3_11_0, v3_12_0, v3_13_0, v3_6_6, v3_7_0, v3_8_0, v3_9_5};
use crate::python_data_access::{copy_long, copy_string, DictIterator, PY_TPFLAGS_MANAGED_DICT};
use crate::python_interpreters::{InterpreterState, Object, TypeObject};
use crate::python_spy::PythonSpy;
use remoteprocess::Process;

use crate::version::Version;

use remoteprocess::ProcessMemory;

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
pub fn thread_names_from_interpreter<I: InterpreterState, P: ProcessMemory>(
    interpreter_address: usize,
    process: &P,
    version: &Version,
) -> Result<HashMap<u64, String>, Error> {
    let modules_ptr_ptr = I::modules_ptr_ptr(interpreter_address);
    let modules: *const I::Object = process
        .copy_pointer(modules_ptr_ptr)
        .context("Failed to copy modules PyObject")?;
    thread_names_from_modules::<I, P>(modules as usize, process, version)
}

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
fn _thread_name_lookup<I: InterpreterState>(
    spy: &PythonSpy,
) -> Result<HashMap<u64, String>, Error> {
    thread_names_from_interpreter::<I, Process>(spy.interpreter_address, &spy.process, &spy.version)
}

// try getting the threadnames, but don't sweat it if we can't. Since this relies on dictionary
// processing we only handle py3.6+ right now, and this doesn't work at all if the
// threading module isn't imported in the target program
pub fn thread_name_lookup(process: &PythonSpy) -> Option<HashMap<u64, String>> {
    let err = match process.version {
        Version {
            major: 3, minor: 6, ..
        } => _thread_name_lookup::<v3_6_6::_is>(process),
        Version {
            major: 3, minor: 7, ..
        } => _thread_name_lookup::<v3_7_0::_is>(process),
        Version {
            major: 3, minor: 8, ..
        } => _thread_name_lookup::<v3_8_0::_is>(process),
        Version {
            major: 3, minor: 9, ..
        } => _thread_name_lookup::<v3_9_5::_is>(process),
        Version {
            major: 3,
            minor: 10,
            ..
        } => _thread_name_lookup::<v3_10_0::_is>(process),
        Version {
            major: 3,
            minor: 11,
            ..
        } => _thread_name_lookup::<v3_11_0::_is>(process),
        Version {
            major: 3,
            minor: 12,
            ..
        } => _thread_name_lookup::<v3_12_0::_is>(process),
        Version {
            major: 3,
            minor: 13,
            ..
        } => _thread_name_lookup::<v3_13_0::_is>(process),
        Version {
            major: 3,
            minor: 14,
            ..
        } => _thread_name_lookup_via_offsets(process),
        _ => return None,
    };
    if let Err(ref e) = err {
        warn!("thread_name_lookup: {:?}", e);
    }
    err.ok()
}

/// Fully offset-based thread name lookup for 3.14+ (handles both GIL-enabled and free-threaded).
fn _thread_name_lookup_via_offsets(spy: &PythonSpy) -> Result<HashMap<u64, String>, Error> {
    use crate::offset_stack_trace::{iter_dict_via_offsets, read_unicode_string};

    let offsets = spy
        .debug_offsets
        .as_ref()
        .ok_or_else(|| anyhow::format_err!("debug offsets not available"))?;

    let modules_addr: usize = spy.process.copy_struct(
        spy.interpreter_address + offsets.interp_imports_modules as usize,
    )?;

    let modules_entries = iter_dict_via_offsets(&spy.process, modules_addr, offsets)?;

    // Discover type object field offsets from the first module in sys.modules.
    let type_offsets = modules_entries
        .first()
        .and_then(|&(_, v)| discover_type_offsets(&spy.process, v, offsets).ok())
        .unwrap_or(TypeOffsets {
            tp_dictoffset: 0,
            tp_basicsize: offsets.type_object_tp_name + std::mem::size_of::<usize>() as u64,
            ht_cached_keys: 0,
        });

    let mut ret = HashMap::new();
    for (key, value) in modules_entries {
        let module_name = read_unicode_string(&spy.process, key, offsets)?;
        if module_name != "threading" {
            continue;
        }

        let module_dict = read_object_dict(&spy.process, value, offsets, &type_offsets)?;
        if module_dict == 0 {
            continue;
        }

        let module_dict_entries = iter_dict_via_offsets(&spy.process, module_dict, offsets)?;
        info!("threading module dict: {} entries", module_dict_entries.len());
        for (key, value) in module_dict_entries {
            let name = read_unicode_string(&spy.process, key, offsets)?;
            if name != "_active" {
                continue;
            }

            for (key, value) in iter_dict_via_offsets(&spy.process, value, offsets)? {
                let (threadid, _) = crate::offset_stack_trace::read_long_via_offsets(
                    &spy.process,
                    offsets,
                    key,
                )?;

                let thread_type_addr =
                    crate::debug_offsets::read_ptr(&spy.process, value, offsets.pyobject_ob_type)?;
                let thread_dict =
                    read_object_dict(&spy.process, value, offsets, &type_offsets)?;
                if thread_dict != 0 {
                    for (k, v) in iter_dict_via_offsets(&spy.process, thread_dict, offsets)? {
                        let varname = read_unicode_string(&spy.process, k, offsets)?;
                        if varname == "_name" {
                            let threadname = read_unicode_string(&spy.process, v, offsets)?;
                            ret.insert(threadid as u64, threadname);
                            break;
                        }
                    }
                } else {
                    // Dict not materialized — try inline values
                    let name_addr = read_inline_attr(
                        &spy.process,
                        value,
                        "_name",
                        thread_type_addr,
                        offsets,
                        &type_offsets,
                    )?;
                    if name_addr != 0 {
                        if let Ok(threadname) = read_unicode_string(&spy.process, name_addr, offsets) {
                            ret.insert(threadid as u64, threadname);
                        }
                    }
                }
            }
            break;
        }
        break;
    }
    Ok(ret)
}

/// Discovered offsets within PyTypeObject/PyHeapTypeObject that aren't in _Py_DebugOffsets.
struct TypeOffsets {
    tp_dictoffset: u64,
    tp_basicsize: u64,
    ht_cached_keys: u64,
}

/// Compute PyTypeObject field offsets that aren't in _Py_DebugOffsets.
///
/// These offsets are derived from the debug offsets plus known invariants:
///
/// - tp_basicsize immediately follows tp_name in PyTypeObject.
/// - tp_dictoffset is discovered by scanning a module type for
///   tp_dictoffset == pyobject_size (since PyModuleObject has md_dict
///   right after its PyObject header).
/// - ht_cached_keys uses the 3.13 bindgen offset (GIL-enabled layout is
///   identical between 3.13 and 3.14), adjusted for free-threaded builds
///   by the PyObject header size delta from _Py_DebugOffsets.pyobject.size.
fn discover_type_offsets<P: ProcessMemory>(
    process: &P,
    module_obj_addr: usize,
    offsets: &crate::debug_offsets::DebugOffsets,
) -> Result<TypeOffsets, anyhow::Error> {
    use crate::debug_offsets::read_ptr;

    let ptr_size = std::mem::size_of::<usize>();
    let type_addr = read_ptr(process, module_obj_addr, offsets.pyobject_ob_type)?;

    // tp_basicsize immediately follows tp_name in PyTypeObject
    let tp_basicsize = offsets.type_object_tp_name + ptr_size as u64;

    // tp_dictoffset: scan for pyobject_size (module's tp_dictoffset == pyobject_size)
    let expected_dictoffset = offsets.pyobject_size;
    let scan_size = (offsets.type_object_tp_flags + 256) as usize;
    let type_buf = process.copy(type_addr, scan_size)?;
    let start = offsets.type_object_tp_flags as usize + ptr_size;
    let mut tp_dictoffset = 0u64;
    for off in (start..type_buf.len().saturating_sub(ptr_size - 1)).step_by(ptr_size) {
        let val = usize::from_ne_bytes(type_buf[off..off + ptr_size].try_into().unwrap());
        if val as u64 == expected_dictoffset {
            tp_dictoffset = off as u64;
            break;
        }
    }

    // ht_cached_keys: use the 3.13 bindgen offset (correct for GIL-enabled 3.14 —
    // PyHeapTypeObject layout is identical between 3.13 and 3.14 GIL-enabled).
    // For free-threaded builds, adjust by the PyObject header size delta, since
    // PyObject_VAR_HEAD at the root of PyTypeObject shifts all subsequent fields.
    let gil_offset = std::mem::offset_of!(
        crate::python_bindings::v3_13_0::PyHeapTypeObject,
        ht_cached_keys
    ) as u64;
    let ht_cached_keys = if offsets.free_threaded {
        // The GIL-enabled PyObject is 16 bytes; free-threaded is larger.
        // Every field in PyTypeObject/PyHeapTypeObject shifts by the delta.
        let pyobject_delta = offsets.pyobject_size - 16;
        gil_offset + pyobject_delta
    } else {
        gil_offset
    };

    Ok(TypeOffsets {
        tp_dictoffset,
        tp_basicsize,
        ht_cached_keys,
    })
}

/// Read an object's __dict__ using debug offsets.
/// Handles: managed dicts, inline values, and legacy tp_dictoffset.
fn read_object_dict<P: ProcessMemory>(
    process: &P,
    obj_addr: usize,
    offsets: &crate::debug_offsets::DebugOffsets,
    type_offsets: &TypeOffsets,
) -> Result<usize, anyhow::Error> {
    use crate::debug_offsets::read_ptr;

    let type_addr = read_ptr(process, obj_addr, offsets.pyobject_ob_type)?;
    let flags = read_ptr(process, type_addr, offsets.type_object_tp_flags)?;

    if flags & PY_TPFLAGS_MANAGED_DICT != 0 {
        let ptr_size = std::mem::size_of::<usize>();
        let managed_offset: isize = if offsets.free_threaded { -1 } else { -3 };
        let dict_ptr_addr = (obj_addr as isize + managed_offset * ptr_size as isize) as usize;
        let dict_addr: usize = process.copy_struct(dict_ptr_addr)?;
        Ok(dict_addr)
    } else if type_offsets.tp_dictoffset != 0 {
        let dictoffset: isize =
            process.copy_struct(type_addr + type_offsets.tp_dictoffset as usize)?;
        if dictoffset == 0 {
            return Ok(0);
        }
        let dict_addr: usize = process.copy_struct((obj_addr as isize + dictoffset) as usize)?;
        Ok(dict_addr)
    } else {
        Ok(0)
    }
}

/// Read an object's attribute from inline values (when managed dict is null).
/// Returns the value of the named attribute, or 0 if not found.
fn read_inline_attr<P: ProcessMemory>(
    process: &P,
    obj_addr: usize,
    attr_name: &str,
    type_addr: usize,
    offsets: &crate::debug_offsets::DebugOffsets,
    type_offsets: &TypeOffsets,
) -> Result<usize, anyhow::Error> {
    use crate::debug_offsets::read_ptr;

    if type_offsets.ht_cached_keys == 0 || type_offsets.tp_basicsize == 0 {
        return Ok(0);
    }

    let tp_basicsize: usize = process.copy_struct(type_addr + type_offsets.tp_basicsize as usize)?;
    let ht_cached_keys: usize =
        process.copy_struct(type_addr + type_offsets.ht_cached_keys as usize)?;
    if ht_cached_keys == 0 {
        return Ok(0);
    }

    let keys: crate::python_bindings::v3_12_0::PyDictKeysObject =
        process.copy_struct(ht_cached_keys)?;
    let entries_addr =
        ht_cached_keys + (1 << keys.dk_log2_index_bytes) + std::mem::size_of_val(&keys);

    let ptr_size = std::mem::size_of::<usize>();
    // Inline values start after the object + _dictvalues header.
    // _dictvalues has a 4-byte header (capacity, size, embedded, valid),
    // padded to pointer alignment (8 bytes on 64-bit).
    // See cpython/Include/internal/pycore_dict.h struct _dictvalues.
    let dictvalues_header = std::mem::size_of::<usize>(); // 4 bytes padded to ptr alignment
    let values_addr = obj_addr + tp_basicsize + dictvalues_header;

    for index in 0..keys.dk_nentries as usize {
        let entry_size = if keys.dk_kind == 0 { 3 * ptr_size } else { 2 * ptr_size };
        let entry_addr = entries_addr + index * entry_size;
        let key_ptr = if keys.dk_kind == 0 {
            read_ptr(process, entry_addr + ptr_size, 0)?
        } else {
            read_ptr(process, entry_addr, 0)?
        };
        if key_ptr == 0 {
            continue;
        }
        if let Ok(name) = crate::offset_stack_trace::read_unicode_string(process, key_ptr, offsets)
        {
            if name == attr_name {
                let val: usize = process.copy_struct(values_addr + index * ptr_size)?;
                return Ok(val);
            }
        }
    }
    Ok(0)
}

/// Like thread_names_from_interpreter but takes the modules dict address directly.
fn thread_names_from_modules<I: InterpreterState, P: ProcessMemory>(
    modules_addr: usize,
    process: &P,
    version: &Version,
) -> Result<HashMap<u64, String>, Error> {
    let mut ret = HashMap::new();
    for entry in DictIterator::from(process, version, modules_addr)? {
        let (key, value) = entry?;
        let module_name = copy_string(key as *const I::StringObject, process)?;
        if module_name == "threading" {
            let module: I::Object = process.copy_struct(value)?;
            let module_type = process.copy_pointer(module.ob_type())?;
            let dictptr: usize = process.copy_struct(value + module_type.dictoffset() as usize)?;
            for i in DictIterator::from(process, version, dictptr)? {
                let (key, value) = i?;
                let name = copy_string(key as *const I::StringObject, process)?;
                if name == "_active" {
                    for i in DictIterator::from(process, version, value)? {
                        let (key, value) = i?;
                        let (threadid, _) = copy_long(process, version, key)?;

                        let thread: I::Object = process.copy_struct(value)?;
                        let thread_type = process.copy_pointer(thread.ob_type())?;
                        let flags = thread_type.flags();

                        let dict_iter = if flags & PY_TPFLAGS_MANAGED_DICT != 0 {
                            DictIterator::from_managed_dict(
                                process,
                                version,
                                value,
                                thread.ob_type() as usize,
                                flags,
                            )?
                        } else {
                            let dict_offset = thread_type.dictoffset();
                            let dict_addr = (value as isize + dict_offset) as usize;
                            let thread_dict_addr: usize = process.copy_struct(dict_addr)?;
                            DictIterator::from(process, version, thread_dict_addr)?
                        };

                        for i in dict_iter {
                            let (key, value) = i?;
                            let varname = copy_string(key as *const I::StringObject, process)?;
                            if varname == "_name" {
                                let threadname =
                                    copy_string(value as *const I::StringObject, process)?;
                                ret.insert(threadid as u64, threadname);
                                break;
                            }
                        }
                    }
                    break;
                }
            }
            break;
        }
    }
    Ok(ret)
}
