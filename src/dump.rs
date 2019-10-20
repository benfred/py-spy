use std::collections::HashMap;

use console::style;
use failure::Error;

use crate::config::Config;
use crate::python_bindings::{v3_6_6, v3_7_0, v3_8_0};
use crate::python_interpreters::{InterpreterState, Object, TypeObject};
use crate::python_spy::PythonSpy;
use crate::python_data_access::{copy_string, copy_long, DictIterator};

use crate::version::Version;

use remoteprocess::{ProcessMemory, Pid};

pub fn print_traces(pid: Pid, config: &Config) -> Result<(), Error> {
    let mut process = PythonSpy::new(pid, config)?;
    if config.dump_json {
        let traces = process.get_stack_traces()?;
        println!("{}", serde_json::to_string_pretty(&traces)?);
        return Ok(())
    }

    // try getting the threadnames, but don't sweat it if we can't. Since this relies on dictionary
    // processing we only handle py3.6+ right now, and this doesn't work at all if the
    // threading module isn't imported in the target program
    let thread_names = match process.version {
        Version{major: 3, minor: 6, ..} => thread_name_lookup::<v3_6_6::_is>(&process).ok(),
        Version{major: 3, minor: 7, ..} => thread_name_lookup::<v3_7_0::_is>(&process).ok(),
        Version{major: 3, minor: 8, ..} => thread_name_lookup::<v3_8_0::_is>(&process).ok(),
        _ => None
    };

    println!("Process {}: {}",
        style(process.pid).bold().yellow(),
        process.process.cmdline()?.join(" "));

    println!("Python v{} ({})\n",
        style(&process.version).bold(),
        style(process.process.exe()?).dim());

    let traces = process.get_stack_traces()?;

    for trace in traces.iter().rev() {
        let thread_id = trace.format_threadid();
        let thread_name = match thread_names.as_ref() {
            Some(names) => names.get(&trace.thread_id),
            None => None
        };
        match thread_name {
            Some(name) => {
                println!("Thread {} ({}): \"{}\"", style(thread_id).bold().yellow(), trace.status_str(), name);
            }
            None => {
                println!("Thread {} ({})", style(thread_id).bold().yellow(), trace.status_str());
            }
        };

        for frame in &trace.frames {
            let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
            if frame.line != 0 {
                println!("    {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
            } else {
                println!("    {} ({})", style(&frame.name).green(), style(&filename).cyan());
            }

            if let Some(locals) = &frame.locals {
                let mut shown_args = false;
                let mut shown_locals = false;
                for local in locals {
                    if local.arg && !shown_args {
                        println!("        {}:", style("Arguments:").dim());
                        shown_args = true;
                    } else if !local.arg && !shown_locals {
                        println!("        {}:", style("Locals:").dim());
                        shown_locals = true;
                    }

                    let repr = local.repr.as_ref().map(String::as_str).unwrap_or("?");
                    println!("            {}: {}", local.name, repr);
                }
            }
        }
    }
    Ok(())
}

/// Returns a hashmap of threadid: threadname, by inspecting the '_active' variable in the
/// 'threading' module.
fn thread_name_lookup<I: InterpreterState>(spy: &PythonSpy) -> Result<HashMap<u64, String>, Error> {
    let mut ret = HashMap::new();
    let process = &spy.process;
    let interp: I = process.copy_struct(spy.interpreter_address)?;
    for entry in DictIterator::from(process, interp.modules() as usize)? {
        let (key, value) = entry?;
        let module_name = copy_string(key as *const I::StringObject, process)?;
        if module_name == "threading" {
            let module: I::Object = process.copy_struct(value)?;
            let module_type = process.copy_pointer(module.ob_type())?;
            let dictptr: usize = process.copy_struct(value + module_type.dictoffset() as usize)?;
            for i in DictIterator::from(process, dictptr)? {
                let (key, value) = i?;
                let name = copy_string(key as *const I::StringObject, process)?;
                if name == "_active" {
                    for i in DictIterator::from(process, value)? {
                        let (key, value) = i?;
                        let (threadid, _) = copy_long(process, key)?;

                        let thread: I::Object = process.copy_struct(value)?;
                        let thread_type = process.copy_pointer(thread.ob_type())?;
                        let thread_dict_addr: usize = process.copy_struct(value + thread_type.dictoffset() as usize)?;

                        for i in DictIterator::from(process, thread_dict_addr)? {
                            let (key, value) = i?;
                            let varname = copy_string(key as *const I::StringObject, process)?;
                            if varname == "_name" {
                                let threadname = copy_string(value as *const I::StringObject, process)?;
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
