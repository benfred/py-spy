use std;

use failure::Error;

use remoteprocess::ProcessMemory;
use crate::python_interpreters::{StringObject, BytesObject, InterpreterState, Object, TypeObject, TupleObject, ListObject};
use crate::version::Version;

/// Copies a string from a target process. Attempts to handle unicode differences, which mostly seems to be working
pub fn copy_string<T: StringObject, P: ProcessMemory>(ptr: * const T, process: &P) -> Result<String, Error> {
    let obj = process.copy_pointer(ptr)?;
    if obj.size() >= 4096 {
        return Err(format_err!("Refusing to copy {} chars of a string", obj.size()));
    }

    let kind = obj.kind();

    let bytes = process.copy(obj.address(ptr as usize), obj.size() * kind as usize)?;

    match (kind, obj.ascii()) {
        (4, _) => {
            #[allow(clippy::cast_ptr_alignment)]
            let chars = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as * const char, bytes.len() / 4) };
            Ok(chars.iter().collect())
        },
        (2, _) => {
            // UCS2 strings aren't used internally after v3.3: https://www.python.org/dev/peps/pep-0393/
            // TODO: however with python 2.7 they could be added with --enable-unicode=ucs2 configure flag.
            //            or with python 3.2 --with-wide-unicode=ucs2
            Err(format_err!("ucs2 strings aren't supported yet!"))
        },
        (1, true) => Ok(String::from_utf8(bytes)?),
        (1, false) => Ok(bytes.iter().map(|&b| { b as char }).collect()),
        _ => Err(format_err!("Unknown string kind {}", kind))
    }
}

/// Copies data from a PyBytesObject (currently only lnotab object)
pub fn copy_bytes<T: BytesObject, P: ProcessMemory>(ptr: * const T, process: &P) -> Result<Vec<u8>, Error> {
    let obj = process.copy_pointer(ptr)?;
    let size = obj.size();
    if size >= 65536 {
        return Err(format_err!("Refusing to copy {} bytes", size));
    }
    Ok(process.copy(obj.address(ptr as usize), size as usize)?)
}

/// Copys a i64 from a PyLongObject. Returns the value + if it overflowed
pub fn copy_long(process: &remoteprocess::Process, addr: usize) -> Result<(i64, bool), Error> {
    // this is PyLongObject for a specific version of python, but this works since it's binary compatible
    // layout across versions we're targetting
    let value = process.copy_pointer(addr as *const crate::python_bindings::v3_7_0::PyLongObject)?;
    let negative: i64 = if value.ob_base.ob_size < 0 { -1 } else { 1 };
    let size = value.ob_base.ob_size * (negative as isize);
    match size {
        0 => Ok((0, false)),
        1 => Ok((negative * (value.ob_digit[0] as i64), false)),

        #[cfg(target_pointer_width = "64")]
        2 => {
            let digits: [u32; 2] = process.copy_struct(addr + std::mem::size_of_val(&value) - 8)?;
            let mut ret: i64 = 0;
            for i in 0..size {
                ret += (digits[i as usize] as i64) << (30 * i);
            }
            Ok((negative * ret, false))
        }
        #[cfg(target_pointer_width = "32")]
        2..=4 => {
            let digits: [u16; 4] = process.copy_struct(addr + std::mem::size_of_val(&value) - 4)?;
            let mut ret: i64 = 0;
            for i in 0..size {
                ret += (digits[i as usize] as i64) << (15 * i);
            }
            Ok((negative * ret, false))
        }
        // we don't support arbitrary sized integers yet, signal this by returning that we've overflowed
        _ => Ok((value.ob_base.ob_size as i64, true))
    }
}

/// Copys a i64 from a python 2.7 PyIntObject
pub fn copy_int(process: &remoteprocess::Process, addr: usize) -> Result<i64, Error> {
    let value = process.copy_pointer(addr as *const crate::python_bindings::v2_7_15::PyIntObject)?;
    Ok(value.ob_ival as i64)
}

/// Allows iteration of a python dictionary. Only supports python 3.6+ right now
pub struct DictIterator<'a> {
    process: &'a remoteprocess::Process,
    entries_addr: usize,
    index: usize,
    entries: usize,
    values: usize
}

impl<'a> DictIterator<'a> {
    pub fn from(process: &'a remoteprocess::Process, addr: usize) -> Result<DictIterator, Error> {
        // Getting this going generically is tricky: there is a lot of variation on how dictionaries are handled
        // instead this just focuses on a single version, which works for python 3.6/3.7/3.8
        let dict: crate::python_bindings::v3_7_0::PyDictObject = process.copy_struct(addr)?;
        let keys = process.copy_pointer(dict.ma_keys)?;
        let index_size = match keys.dk_size {
            0..=0xff => 1,
            0..=0xffff => 2,
            #[cfg(target_pointer_width = "64")]
            0..=0xffffffff => 4,
            #[cfg(target_pointer_width = "64")]
            _ => 8,
            #[cfg(not(target_pointer_width = "64"))]
            _ => 4
        };
        let byteoffset = (keys.dk_size * index_size) as usize;
        let entries_addr = dict.ma_keys as usize + byteoffset + std::mem::size_of_val(&keys);
        Ok(DictIterator{process, entries_addr, index: 0, entries: keys.dk_nentries as usize, values: dict.ma_values as usize})
    }
}

impl<'a> Iterator for DictIterator<'a> {
    type Item = Result<(usize, usize), Error>;
    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.entries {
            let addr = self.index* std::mem::size_of::<crate::python_bindings::v3_7_0::PyDictKeyEntry>() + self.entries_addr;
            self.index += 1;
            let entry: Result<crate::python_bindings::v3_7_0::PyDictKeyEntry, remoteprocess::Error> = self.process.copy_struct(addr);
            match entry {
                Ok(entry) => {
                    if entry.me_key.is_null() {
                        continue;
                    }

                    let value = if self.values != 0 {
                        let valueaddr = self.values + (self.index - 1) * std::mem::size_of::<* mut crate::python_bindings::v3_7_0::PyObject>();
                        match self.process.copy_struct(valueaddr) {
                            Ok(addr) => addr,
                            Err(e) => { return Some(Err(e.into())); }
                        }
                    } else {
                        entry.me_value as usize
                    };

                    return Some(Ok((entry.me_key as usize, value)))
                },
                Err(e) => {
                    return Some(Err(e.into()))
                }
            }
        }

        None
    }
}

const PY_TPFLAGS_INT_SUBCLASS: usize =     1 << 23;
const PY_TPFLAGS_LONG_SUBCLASS: usize =    1 << 24;
const PY_TPFLAGS_LIST_SUBCLASS: usize =    1 << 25;
const PY_TPFLAGS_TUPLE_SUBCLASS: usize =   1 << 26;
const PY_TPFLAGS_BYTES_SUBCLASS: usize =   1 << 27;
const PY_TPFLAGS_STRING_SUBCLASS: usize = 1 << 28;
const PY_TPFLAGS_DICT_SUBCLASS: usize =    1 << 29;

/// Converts a python variable in the other process to a human readable string
pub fn format_variable<I>(process: &remoteprocess::Process, version: &Version, addr: usize, max_length: isize)
        -> Result<String, Error> where I: InterpreterState {
    // We need at least 5 characters remaining for all this code to work, replace with an ellipsis if
    // we're out of space
    if max_length <= 5 {
        return Ok("...".to_owned());
    }

    let value: I::Object = process.copy_struct(addr)?;
    let value_type = process.copy_pointer(value.ob_type())?;

    // get the typename (truncating to 128 bytes if longer)
    let max_type_len = 128;
    let value_type_name = process.copy(value_type.name() as usize, max_type_len)?;
    let length = value_type_name.iter().position(|&x| x == 0).unwrap_or(max_type_len);
    let value_type_name = std::str::from_utf8(&value_type_name[..length])?;

    let format_int = |value: i64| {
        if value_type_name == "bool" {
            (if value > 0 { "True" } else { "False" }).to_owned()
        } else {
            format!("{}", value)
        }
    };

    // use the flags/typename to figure out how to stringify this object
    let flags = value_type.flags();
    let formatted = if flags & PY_TPFLAGS_INT_SUBCLASS != 0 {
        format_int(copy_int(process, addr)?)
    } else if flags & PY_TPFLAGS_LONG_SUBCLASS != 0 {
        // we don't handle arbitray sized integer values (max is 2**60)
        let (value, overflowed) = copy_long(process, addr)?;
         if overflowed {
            if value > 0 { "+bigint".to_owned() } else { "-bigint".to_owned() }
        } else {
            format_int(value)
        }
    } else if flags & PY_TPFLAGS_STRING_SUBCLASS != 0 ||
            (version.major ==  2 && (flags & PY_TPFLAGS_BYTES_SUBCLASS != 0)) {
        let value = copy_string(addr as *const I::StringObject, process)?.replace("\"", "\\\"").replace("\n", "\\n");
        if value.len() as isize >= max_length - 5 {
            format!("\"{}...\"", &value[..(max_length - 5) as usize])
        } else {
            format!("\"{}\"", value)
        }
    } else if flags & PY_TPFLAGS_DICT_SUBCLASS != 0 {
        if version.major == 3 && version.minor >= 6 {
            let mut values = Vec::new();
            let mut remaining = max_length - 2;
            for entry in DictIterator::from(process, addr)? {
                let (key, value) = entry?;
                let key = format_variable::<I>(process, version, key, remaining)?;
                let value = format_variable::<I>(process, version, value, remaining)?;
                remaining -= (key.len() + value.len()) as isize + 4;
                if remaining <= 5 {
                    values.push("...".to_owned());
                    break;
                }
                values.push(format!("{}: {}", key, value));
            }
            format!("{{{}}}", values.join(", "))
        } else {
            // TODO: support getting dictionaries from older versions of python
            "dict".to_owned()
        }
    } else if flags & PY_TPFLAGS_LIST_SUBCLASS != 0 {
        let object: I::ListObject = process.copy_struct(addr)?;
        let addr = object.item() as usize;
        let mut values = Vec::new();
        let mut remaining = max_length - 2;
        for i in 0..object.size() {
            let valueptr: *mut I::Object = process.copy_struct(addr + i * std::mem::size_of::<* mut I::Object>())?;
            let value = format_variable::<I>(process, version, valueptr as usize, remaining)?;
            remaining -= value.len() as isize + 2;
            if remaining <= 5 {
                values.push("...".to_owned());
                break;
            }
            values.push(value);
        }
        format!("[{}]", values.join(", "))
    } else if flags & PY_TPFLAGS_TUPLE_SUBCLASS != 0 {
        let object: I::TupleObject = process.copy_struct(addr)?;
        let mut values = Vec::new();
        let mut remaining = max_length - 2;
        for i in 0..object.size() {
            let value_addr: *mut I::Object = process.copy_struct(object.address(addr, i))?;
            let value = format_variable::<I>(process, version, value_addr as usize, remaining)?;
            remaining -= value.len() as isize + 2;
            if remaining <= 5 {
                values.push("...".to_owned());
                break;
            }
            values.push(value);
        }
        format!("({})", values.join(", "))
    } else if value_type_name == "float" {
        let value = process.copy_pointer(addr as *const crate::python_bindings::v3_7_0::PyFloatObject)?;
        format!("{}", value.ob_fval)
    } else if value_type_name == "NoneType" {
        "None".to_owned()
    } else {
        format!("<{} at 0x{:x}>", value_type_name, addr)
    };

    Ok(formatted)
}

#[cfg(test)]
pub mod tests {
    // the idea here is to create various cpython interpretator structs locally
    // and then test out that the above code handles appropiately
    use super::*;
    use remoteprocess::LocalProcess;
    use crate::python_bindings::v3_7_0::{PyBytesObject, PyVarObject, PyUnicodeObject, PyASCIIObject};
    use std::ptr::copy_nonoverlapping;

    // python stores data after pybytesobject/pyasciiobject. hack by initializing a 4k buffer for testing.
    // TODO: get better at Rust and figure out a better solution
    #[allow(dead_code)]
    pub struct AllocatedPyByteObject {
        pub base: PyBytesObject,
        pub storage: [u8; 4096]
    }

    #[allow(dead_code)]
    pub struct AllocatedPyASCIIObject {
        pub base: PyASCIIObject,
        pub storage: [u8; 4096]
    }

    pub fn to_byteobject(bytes: &[u8]) -> AllocatedPyByteObject {
        let ob_size = bytes.len() as isize;
        let base = PyBytesObject{ob_base: PyVarObject{ob_size, ..Default::default()}, ..Default::default()};
        let mut ret = AllocatedPyByteObject{base, storage: [0 as u8; 4096]};
        unsafe { copy_nonoverlapping(bytes.as_ptr(), ret.base.ob_sval.as_mut_ptr() as *mut u8, bytes.len()); }
        ret
    }

    pub fn to_asciiobject(input: &str) -> AllocatedPyASCIIObject {
        let bytes: Vec<u8> = input.bytes().collect();
        let mut base = PyASCIIObject{length: bytes.len() as isize, ..Default::default()};
        base.state.set_compact(1);
        base.state.set_kind(1);
        base.state.set_ascii(1);
        let mut ret = AllocatedPyASCIIObject{base, storage: [0 as u8; 4096]};
        unsafe {
            let ptr = &mut ret as *mut AllocatedPyASCIIObject as *mut u8;
            let dst = ptr.offset(std::mem::size_of::<PyASCIIObject>() as isize);
            copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }
        ret
    }

    #[test]
    fn test_copy_string() {
        let original = "function_name";
        let obj = to_asciiobject(original);

        let unicode: &PyUnicodeObject = unsafe{ std::mem::transmute(&obj.base) };
        let copied = copy_string(unicode, &LocalProcess).unwrap();
        assert_eq!(copied, original);
    }

    #[test]
    fn test_copy_bytes() {
        let original = [10_u8, 20, 30, 40, 50, 70, 80];
        let bytes = to_byteobject(&original);
        let copied = copy_bytes(&bytes.base, &LocalProcess).unwrap();
        assert_eq!(copied, original);
    }
}
