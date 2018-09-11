use std;

use failure::{Error, ResultExt};
use read_process_memory::{CopyAddress, copy_address};

use python_interpreters::{InterpreterState, ThreadState, FrameObject, CodeObject, StringObject, BytesObject};
use utils::{copy_pointer};

#[derive(Debug)]
pub struct StackTrace {
    pub thread_id: u64,
    pub active: bool,
    pub owns_gil: bool,
    pub frames: Vec<Frame>
}

#[derive(Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Clone)]
pub struct Frame {
    pub name: String,
    pub filename: String,
    pub short_filename: Option<String>,
    pub line: i32
}

/// Given an InterpreterState, this function returns a vector of stack traces for each thread
pub fn get_stack_traces<I, P>(interpreter: &I, process: &P) -> Result<(Vec<StackTrace>), Error>
        where I: InterpreterState, P: CopyAddress {
    let mut ret = Vec::new();
    let mut threads = interpreter.head();
    while !threads.is_null() {
        let thread = copy_pointer(threads, process).context("Failed to copy PyThreadState")?;
        ret.push(get_stack_trace(&thread, process)?);
        // This seems to happen occasionally when scanning BSS addresses for valid interpeters
        if ret.len() > 4096 {
            return Err(format_err!("Max thread recursion depth reached"));
        }
        threads = thread.next();
    }
    Ok(ret)
}

/// Gets a stack trace for an individual thread
pub fn get_stack_trace<T, P >(thread: &T, process: &P) -> Result<StackTrace, Error>
        where T: ThreadState, P: CopyAddress {
    let mut frames = Vec::new();
    let mut frame_ptr = thread.frame();
    while !frame_ptr.is_null() {
        let frame = copy_pointer(frame_ptr, process).context("Failed to copy PyFrameObject")?;
        let code = copy_pointer(frame.code(), process).context("Failed to copy PyCodeObject")?;

        let filename = copy_string(code.filename(), process).context("Failed to copy filename")?;
        let name = copy_string(code.name(), process).context("Failed to copy function name")?;
        let line = get_line_number(&code, frame.lasti(), process).context("Failed to get line number")?;

        frames.push(Frame{name, filename, line, short_filename: None});
        if frames.len() > 4096 {
            return Err(format_err!("Max frame recursion depth reached"));
        }

        frame_ptr = frame.back();
    }

    // figure out if the thread is running
    let idle = if frames.is_empty() {
        true
    } else {
        // TODO: better idle detection. This is just hackily looking at the
        // function/file to figure out if the thread is waiting (which seems to handle
        // most cases)
        let frame = &frames[0];
        (frame.name == "wait" && frame.filename.ends_with("threading.py")) ||
        (frame.name == "select" && frame.filename.ends_with("selectors.py")) ||
        (frame.name == "poll" && (frame.filename.ends_with("asyncore.py") ||
                                  frame.filename.contains("zmq") ||
                                  frame.filename.contains("gevent") ||
                                  frame.filename.contains("tornado")))
    };

    Ok(StackTrace{frames, thread_id: thread.thread_id(), owns_gil: false, active: !idle})
}

impl StackTrace {
    pub fn status_str(&self) -> &str {
        match (self.owns_gil, self.active) {
            (_, false) => "idle",
            (true, true) => "active+gil",
            (false, true) => "active",
        }
    }
}

/// Returns the line number from a PyCodeObject (given the lasti index from a PyFrameObject)
fn get_line_number<C: CodeObject, P: CopyAddress>(code: &C, lasti: i32, process: &P) -> Result<i32, Error> {
    let table = copy_bytes(code.lnotab(), process).context("Failed to copy line number table")?;

    // unpack the line table. format is specified here:
    // https://github.com/python/cpython/blob/master/Objects/lnotab_notes.txt
    let size = table.len();
    let mut i = 0;
    let mut line_number: i32 = code.first_lineno();
    let mut bytecode_address: i32 = 0;
    while (i + 1) < size {
        bytecode_address += i32::from(table[i]);
        if bytecode_address > lasti {
            break;
        }

        line_number += i32::from(table[i + 1]);
        i += 2;
    }

    Ok(line_number)
}

/// Copies a string from a target process. Attempts to handle unicode differences, which mostly seems to be working
pub fn copy_string<T: StringObject, P: CopyAddress>(ptr: * const T, process: &P) -> Result<String, Error> {
    let obj = copy_pointer(ptr, process)?;
    if obj.size() >= 4096 {
        return Err(format_err!("Refusing to copy {} chars of a string", obj.size()));
    }

    let kind = obj.kind();

    let bytes = copy_address(obj.address(ptr as usize), obj.size() * kind as usize, process)?;

    match (kind, obj.ascii()) {
        (4, _) => {
            #[cfg_attr(feature = "cargo-clippy", allow(cast_ptr_alignment))]
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
pub fn copy_bytes<T: BytesObject, P: CopyAddress>(ptr: * const T, process: &P) -> Result<Vec<u8>, Error> {
    let obj = copy_pointer(ptr, process)?;
    let size = obj.size();
    if size >= 8192 {
        return Err(format_err!("Refusing to copy {} bytes", size));
    }
    Ok(copy_address(obj.address(ptr as usize), size as usize, process)?)
}

#[cfg(test)]
mod tests {
    // the idea here is to create various cpython interpretator structs locally
    // and then test out that the above code handles appropiately
    use super::*;
    use utils::tests::LocalProcess;
    use python_bindings::v3_7_0::{PyCodeObject, PyBytesObject, PyVarObject, PyASCIIObject};
    use std::ptr::copy_nonoverlapping;

    // python stores data after pybytesobject/pyasciiobject. hack by initializing a 4k buffer for testing.
    // TODO: get better at Rust and figure out a better solution
    #[allow(dead_code)]
    struct AllocatedPyByteObject {
        base: PyBytesObject,
        storage: [u8; 4096]
    }

    #[allow(dead_code)]
    struct AllocatedPyASCIIObject {
        base: PyASCIIObject,
        storage: [u8; 4096]
    }

    fn to_byteobject(bytes: &[u8]) -> AllocatedPyByteObject {
        let ob_size = bytes.len() as isize;
        let base = PyBytesObject{ob_base: PyVarObject{ob_size, ..Default::default()}, ..Default::default()};
        let mut ret = AllocatedPyByteObject{base, storage: [0 as u8; 4096]};
        unsafe { copy_nonoverlapping(bytes.as_ptr(), ret.base.ob_sval.as_mut_ptr() as *mut u8, bytes.len()); }
        ret
    }

    fn to_asciiobject(input: &str) -> AllocatedPyASCIIObject {
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
    fn test_get_line_number() {
        let mut lnotab = to_byteobject(&[0u8, 1, 10, 1, 8, 1, 4, 1]);
        let code = PyCodeObject{co_firstlineno: 3,
                                co_lnotab: &mut lnotab.base.ob_base.ob_base,
                                ..Default::default()};
        let lineno = get_line_number(&code, 30, &LocalProcess).unwrap();
        assert_eq!(lineno, 7);
    }

    #[test]
    fn test_copy_string() {
        let original = "function_name";
        let obj = to_asciiobject(original);
        let copied = copy_string(&obj.base, &LocalProcess).unwrap();
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
