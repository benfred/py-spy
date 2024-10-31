use std::collections::HashMap;

use anyhow::Error;

use crate::python_bindings::{v3_10_0, v3_11_0, v3_12_0, v3_13_0, v3_6_6, v3_7_0, v3_8_0, v3_9_5};
use crate::python_data_access::{copy_long, copy_string, DictIterator, PY_TPFLAGS_MANAGED_DICT};
use crate::python_interpreters::{InterpreterState, Object, TypeObject};
use crate::python_spy::PythonSpy;

use crate::version::Version;

use remoteprocess::ProcessMemory;

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
pub fn thread_names_from_interpreter<I: InterpreterState, P: ProcessMemory>(
    interp: &I,
    process: &P,
    version: &Version,
) -> Result<HashMap<u64, String>, Error> {
    let mut ret = HashMap::new();
    for entry in DictIterator::from(process, version, interp.modules() as usize)? {
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

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
fn _thread_name_lookup<I: InterpreterState>(
    spy: &PythonSpy,
) -> Result<HashMap<u64, String>, Error> {
    let interp: I = spy.process.copy_struct(spy.interpreter_address)?;
    thread_names_from_interpreter(&interp, &spy.process, &spy.version)
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
        _ => return None,
    };
    err.ok()
}
