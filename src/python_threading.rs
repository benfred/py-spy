use std::collections::HashMap;

use anyhow::{Context, Error};

use crate::python_data_access::{copy_long, copy_string, DictIterator, PY_TPFLAGS_MANAGED_DICT};
use crate::python_interpreters::{dispatch_by_version, InterpreterState, Object, TypeObject};
use crate::python_process_info::PythonProcessLayout;
use crate::python_spy::PythonSpy;
use remoteprocess::Process;

use crate::version::Version;

use remoteprocess::ProcessMemory;

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
pub fn thread_names_from_interpreter<I: InterpreterState, P: ProcessMemory>(
    layout: &PythonProcessLayout,
    process: &P,
    version: &Version,
) -> Result<HashMap<u64, String>, Error> {
    let modules_ptr_ptr = layout.imports_modules_address as *const *const I::Object;

    let modules: *const I::Object = process
        .copy_pointer(modules_ptr_ptr)
        .context("Failed to copy modules PyObject")?;
    let mut ret = HashMap::new();
    for entry in DictIterator::from(process, version, modules as usize, layout)? {
        let (key, value) = entry?;
        let module_name = copy_string(key as *const I::StringObject, process, &layout.type_checks)?;
        if module_name == "threading" {
            let module: I::Object = process.copy_struct(value)?;
            let module_type = process.copy_pointer(module.ob_type())?;
            let dictptr: usize = process.copy_struct(value + module_type.dictoffset() as usize)?;
            for i in DictIterator::from(process, version, dictptr, &layout)? {
                let (key, value) = i?;
                let name =
                    copy_string(key as *const I::StringObject, process, &layout.type_checks)?;
                if name == "_active" {
                    for i in DictIterator::from(process, version, value, &layout)? {
                        let (key, value) = i?;
                        let (threadid, _) = copy_long(process, version, layout, key)?;

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
                                &layout,
                            )?
                        } else {
                            let dict_offset = thread_type.dictoffset();
                            let dict_addr = (value as isize + dict_offset) as usize;
                            let thread_dict_addr: usize = process.copy_struct(dict_addr)?;
                            DictIterator::from(process, version, thread_dict_addr, &layout)?
                        };

                        for i in dict_iter {
                            let (key, value) = i?;
                            let varname = copy_string(
                                key as *const I::StringObject,
                                process,
                                &layout.type_checks,
                            )?;
                            if varname == "_name" {
                                let threadname = copy_string(
                                    value as *const I::StringObject,
                                    process,
                                    &layout.type_checks,
                                )?;
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

// try getting the threadnames, but don't sweat it if we can't. Since this relies on dictionary
// processing we only handle py3.6+ right now, and this doesn't work at all if the
// threading module isn't imported in the target program
pub fn thread_name_lookup(spy: &PythonSpy) -> Option<HashMap<u64, String>> {
    match spy.version {
        Version {
            major: 3,
            minor: 6..,
            ..
        } => {
            type P = Process;
            let layout = &spy.layout;
            let process = &spy.process;
            let version = &spy.version;
            let err = dispatch_by_version!(
                spy.version,
                spy.layout.free_threaded,
                thread_names_from_interpreter,
                layout,
                process,
                version
            );
            err.ok()
        }
        _ => None,
    }
}
