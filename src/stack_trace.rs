use std::sync::Arc;

use anyhow::{Context, Error, Result};

use remoteprocess::{Pid, ProcessMemory};
use serde_derive::Serialize;

use crate::config::{Config, LineNo};
use crate::python_data_access::{copy_bytes, copy_string};
use crate::python_interpreters::{
    CodeObject, FrameObject, InterpreterState, ThreadState, TupleObject,
};

/// Call stack for a single python thread
#[derive(Debug, Clone, Serialize)]
pub struct StackTrace {
    /// The process id than generated this stack trace
    pub pid: Pid,
    /// The python thread id for this stack trace
    pub thread_id: u64,
    // The python thread name for this stack trace
    pub thread_name: Option<String>,
    /// The OS thread id for this stack tracee
    pub os_thread_id: Option<u64>,
    /// Whether or not the thread was active
    pub active: bool,
    /// Whether or not the thread held the GIL
    pub owns_gil: bool,
    /// The frames
    pub frames: Vec<Frame>,
    /// process commandline / parent process info
    pub process_info: Option<Arc<ProcessInfo>>,
}

/// Information about a single function call in a stack trace
#[derive(Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Clone, Serialize)]
pub struct Frame {
    /// The function name
    pub name: String,
    /// The full filename of the file
    pub filename: String,
    /// The module/shared library the
    pub module: Option<String>,
    /// A short, more readable, representation of the filename
    pub short_filename: Option<String>,
    /// The line number inside the file (or 0 for native frames without line information)
    pub line: i32,
    /// Local Variables associated with the frame
    pub locals: Option<Vec<LocalVariable>>,
    /// If this is an entry frame. Each entry frame corresponds to one native frame.
    pub is_entry: bool,
}

#[derive(Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Clone, Serialize)]
pub struct LocalVariable {
    pub name: String,
    pub addr: usize,
    pub arg: bool,
    pub repr: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub pid: Pid,
    pub command_line: String,
    pub parent: Option<Box<ProcessInfo>>,
}

/// Given an InterpreterState, this function returns a vector of stack traces for each thread
pub fn get_stack_traces<I, P>(
    interpreter: &I,
    process: &P,
    threadstate_address: usize,
    config: Option<&Config>,
) -> Result<Vec<StackTrace>, Error>
where
    I: InterpreterState,
    P: ProcessMemory,
{
    let gil_thread_id = if interpreter.gil_locked().unwrap_or(true) {
        get_gil_threadid::<I, P>(threadstate_address, process)?
    } else {
        0
    };

    let mut ret = Vec::new();
    let mut threads = interpreter.head();

    let lineno = config.map(|c| c.lineno).unwrap_or(LineNo::NoLine);
    let dump_locals = config.map(|c| c.dump_locals).unwrap_or(0);

    while !threads.is_null() {
        let thread = process
            .copy_pointer(threads)
            .context("Failed to copy PyThreadState")?;

        let mut trace = get_stack_trace(&thread, process, dump_locals > 0, lineno)?;
        trace.owns_gil = trace.thread_id == gil_thread_id;

        ret.push(trace);
        // This seems to happen occasionally when scanning BSS addresses for valid interpreters
        if ret.len() > 4096 {
            return Err(format_err!("Max thread recursion depth reached"));
        }
        threads = thread.next();
    }
    Ok(ret)
}

/// Gets a stack trace for an individual thread
pub fn get_stack_trace<T, P>(
    thread: &T,
    process: &P,
    copy_locals: bool,
    lineno: LineNo,
) -> Result<StackTrace, Error>
where
    T: ThreadState,
    P: ProcessMemory,
{
    // TODO: just return frames here? everything else probably should be returned out of scope
    let mut frames = Vec::new();

    // python 3.11+ has an extra level of indirection to get the Frame from the threadstate
    let mut frame_address = thread.frame_address();
    if let Some(addr) = frame_address {
        frame_address = Some(process.copy_struct(addr)?);
    }

    let mut frame_ptr = thread.frame(frame_address);
    while !frame_ptr.is_null() {
        let frame = process
            .copy_pointer(frame_ptr)
            .context("Failed to copy PyFrameObject")?;

        let code = process
            .copy_pointer(frame.code())
            .context("Failed to copy PyCodeObject")?;

        let filename = copy_string(code.filename(), process).context("Failed to copy filename");
        let name = copy_string(code.name(), process).context("Failed to copy function name");

        // just skip processing the current frame if we can't load the filename or function name.
        // this can happen in python 3.13+ since the f_executable isn't guaranteed to be
        // a PyCodeObject. We could check the type (and mimic the logic of PyCode_Check here)
        // but that would require extra overhead of reading the ob_type per frame - and we
        // would also have to figure out what the address of PyCode_Type is (which will be
        // easier if something like https://github.com/python/cpython/issues/100987#issuecomment-1487227139
        // is merged )
        if filename.is_err() || name.is_err() {
            frame_ptr = frame.back();
            continue;
        }
        let filename = filename?;
        let name = name?;

        // skip <shim> entries in python 3.12+
        if filename == "<shim>" {
            frame_ptr = frame.back();
            continue;
        }

        let line = match lineno {
            LineNo::NoLine => 0,
            LineNo::First => code.first_lineno(),
            LineNo::LastInstruction => match get_line_number(&code, frame.lasti(), process) {
                Ok(line) => line,
                Err(e) => {
                    // Failling to get the line number really shouldn't be fatal here, but
                    // can happen in extreme cases (https://github.com/benfred/py-spy/issues/164)
                    // Rather than fail set the linenumber to 0. This is used by the native extensions
                    // to indicate that we can't load a line number and it should be handled gracefully
                    warn!(
                        "Failed to get line number from {}.{}: {}",
                        filename, name, e
                    );
                    0
                }
            },
        };

        let locals = if copy_locals {
            Some(get_locals(&code, frame_ptr, &frame, process)?)
        } else {
            None
        };

        let is_entry = frame.is_entry();

        frames.push(Frame {
            name,
            filename,
            line,
            short_filename: None,
            module: None,
            locals,
            is_entry,
        });
        if frames.len() > 4096 {
            return Err(format_err!("Max frame recursion depth reached"));
        }

        frame_ptr = frame.back();
    }

    Ok(StackTrace {
        pid: 0,
        frames,
        thread_id: thread.thread_id(),
        thread_name: None,
        owns_gil: false,
        active: true,
        os_thread_id: thread.native_thread_id(),
        process_info: None,
    })
}

impl StackTrace {
    pub fn status_str(&self) -> &str {
        match (self.owns_gil, self.active) {
            (_, false) => "idle",
            (true, true) => "active+gil",
            (false, true) => "active",
        }
    }

    pub fn format_threadid(&self) -> String {
        // native threadids in osx are kinda useless, use the pthread id instead
        #[cfg(target_os = "macos")]
        return format!("{:#X}", self.thread_id);

        // otherwise use the native threadid if given
        #[cfg(not(target_os = "macos"))]
        match self.os_thread_id {
            Some(tid) => format!("{}", tid),
            None => format!("{:#X}", self.thread_id),
        }
    }
}

/// Returns the line number from a PyCodeObject (given the lasti index from a PyFrameObject)
fn get_line_number<C: CodeObject, P: ProcessMemory>(
    code: &C,
    lasti: i32,
    process: &P,
) -> Result<i32, Error> {
    let table =
        copy_bytes(code.line_table(), process).context("Failed to copy line number table")?;
    Ok(code.get_line_number(lasti, &table))
}

fn get_locals<C: CodeObject, F: FrameObject, P: ProcessMemory>(
    code: &C,
    frameptr: *const F,
    frame: &F,
    process: &P,
) -> Result<Vec<LocalVariable>, Error> {
    let local_count = code.nlocals() as usize;
    let argcount = code.argcount() as usize;
    let varnames = process.copy_pointer(code.varnames())?;

    let ptr_size = std::mem::size_of::<*const i32>();
    let locals_addr = frameptr as usize + std::mem::size_of_val(frame) - ptr_size;

    let mut ret = Vec::new();

    for i in 0..local_count {
        let nameptr: *const C::StringObject =
            process.copy_struct(varnames.address(code.varnames() as usize, i))?;
        let name = copy_string(nameptr, process)?;
        let addr: usize = process.copy_struct(locals_addr + i * ptr_size)?;
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

pub fn get_gil_threadid<I: InterpreterState, P: ProcessMemory>(
    threadstate_address: usize,
    process: &P,
) -> Result<u64, Error> {
    // figure out what thread has the GIL by inspecting _PyThreadState_Current
    if threadstate_address > 0 {
        let addr: usize = process.copy_struct(threadstate_address)?;

        // if the addr is 0, no thread is currently holding the GIL
        if addr != 0 {
            let threadstate: I::ThreadState = process.copy_struct(addr)?;
            return Ok(threadstate.thread_id());
        }
    }
    Ok(0)
}

impl ProcessInfo {
    pub fn to_frame(&self) -> Frame {
        Frame {
            name: format!("process {}:\"{}\"", self.pid, self.command_line),
            filename: String::from(""),
            module: None,
            short_filename: None,
            line: 0,
            locals: None,
            is_entry: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python_bindings::v3_7_0::PyCodeObject;
    use crate::python_data_access::tests::to_byteobject;
    use remoteprocess::LocalProcess;

    #[test]
    fn test_get_line_number() {
        let mut lnotab = to_byteobject(&[0u8, 1, 10, 1, 8, 1, 4, 1]);
        let code = PyCodeObject {
            co_firstlineno: 3,
            co_lnotab: &mut lnotab.base.ob_base.ob_base,
            ..Default::default()
        };
        let lineno = get_line_number(&code, 30, &LocalProcess).unwrap();
        assert_eq!(lineno, 7);
    }
}
