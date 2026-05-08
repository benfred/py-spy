use anyhow::{Context, Error, Result};

use remoteprocess::ProcessMemory;

use crate::python_data_access::{
    copy_string, format_variable, DictIterator, PY_TPFLAGS_MANAGED_DICT,
};
use crate::python_interpreters::{GcHead, HasGcGenerations, InterpreterState, Object, TypeObject};
use crate::version::Version;

pub fn walk_gc<I, P>(addr: usize, type_name: Option<&str>, process: &P) -> Result<Vec<usize>, Error>
where
    I: HasGcGenerations,
    P: ProcessMemory,
{
    let mut ret = Vec::new();

    for gen in I::gc_generations(addr) {
        let head_addr = I::generation_head_addr(gen);
        let mut gc: I::GcHead = process
            .copy_pointer(head_addr)
            .context("Failed to copy gen gc head")?;
        let mut gc_addr = gc.next();

        while gc_addr as *const _ != head_addr as *const _ {
            gc = process
                .copy_pointer(gc_addr)
                .context("Failed to copy gchead")?;

            let obj_addr = gc.obj_addr(gc_addr as usize);

            let should_add = match type_name {
                Some(name) => isinstance::<I, P>(obj_addr, name, process).unwrap_or(false),
                None => true,
            };
            if should_add {
                ret.push(obj_addr);
            }

            gc_addr = gc.next();
        }
    }
    Ok(ret)
}

fn isinstance<I, P>(addr: usize, type_name: &str, process: &P) -> Result<bool, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let value: I::Object = process.copy_struct(addr)?;
    let value_type = process.copy_pointer(value.ob_type())?;

    // get the typename (truncating to 128 bytes if longer)
    let max_type_len = 128;
    let value_type_name = process.copy(value_type.name() as usize, max_type_len)?;
    let length = value_type_name
        .iter()
        .position(|&x| x == 0)
        .unwrap_or(max_type_len);
    let value_type_name = std::str::from_utf8(&value_type_name[..length])?;

    Ok(value_type_name == type_name)
}

pub fn get_object_attribute<I, P>(
    addr: usize,
    path: &Vec<&str>,
    process: &P,
    version: &Version,
) -> Result<String, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    if path.len() == 0 {
        return Err(format_err!(
            "Path passed to get_object_attribute must not be empty"
        ));
    }

    let mut cur_addr = addr;
    for elem in path {
        match resolve_attribute::<I, P>(cur_addr, elem, process, version) {
            Ok(resolved) => cur_addr = resolved,
            Err(e) => {
                println!("Unable to resolve attribute: {}", e);
                return Err(e);
            }
        };
    }

    format_variable::<I, P>(process, version, cur_addr, 16384)
}

pub fn resolve_attribute<I, P>(
    addr: usize,
    name: &str,
    process: &P,
    version: &Version,
) -> Result<usize, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let value: I::Object = process.copy_struct(addr)?;
    let value_type = process.copy_pointer(value.ob_type())?;

    let flags = value_type.flags();
    let dict_iter = if flags & PY_TPFLAGS_MANAGED_DICT != 0 {
        DictIterator::from_managed_dict(process, version, addr, value.ob_type() as usize, flags)?
    } else {
        let dict_offset = value_type.dictoffset();
        let dict_addr = (addr as isize + dict_offset) as usize;
        let thread_dict_addr: usize = process.copy_struct(dict_addr)?;
        DictIterator::from(process, version, thread_dict_addr)?
    };

    for i in dict_iter {
        let (key, value) = i?;
        let varname = copy_string(key as *const I::StringObject, process)?;
        if varname == name {
            return Ok(value);
        }
    }
    Err(format_err!("Attribute {} not found", name))
}
