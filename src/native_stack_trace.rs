#[cfg(target_os="linux")]
use std::collections::HashSet;
use failure::Error;

use remoteprocess::{Pid};

use crate::binary_parser::BinaryInfo;
use crate::cython;
use crate::stack_trace::{Frame};
use crate::utils::resolve_filename;

pub struct NativeStack {
    should_reload: bool,
    python: BinaryInfo,
    libpython: Option<BinaryInfo>,
    cython_maps: cython::SourceMaps,
    unwinder: remoteprocess::Unwinder,
    // on linux, we also fallback to using libunwind if the main gimli based unwinder fails
    #[cfg(target_os="linux")]
    libunwinder: remoteprocess::LibUnwind,
    // TODO: right now on windows if we don't hold on the process handle unwinding will fail
    #[allow(dead_code)]
    process: remoteprocess::Process,
}

impl NativeStack {
    pub fn new(pid: Pid, python: BinaryInfo, libpython: Option<BinaryInfo>) -> Result<NativeStack, Error> {
        let cython_maps = cython::SourceMaps::new();

        let process = remoteprocess::Process::new(pid)?;
        let unwinder = process.unwinder()?;

        // Try to load up libunwind-ptrace on linux
        #[cfg(target_os="linux")]
        let libunwinder = remoteprocess::libunwind::LibUnwind::new()?;

        return Ok(NativeStack{cython_maps, unwinder, should_reload: false,
                              python,
                              libpython,
                              #[cfg(target_os="linux")]
                              libunwinder,
                              process
                              });
    }
    pub fn merge_native_thread(&mut self, frames: &Vec<Frame>, thread: &remoteprocess::Thread) -> Result<Vec<Frame>, Error> {
        if self.should_reload {
            self.unwinder.reload()?;
            self.should_reload = false;
        }

        // get the native stack from the thread
        #[cfg(not(target_os="linux"))]
        let native_stack = self.get_thread(thread)?;

        // on linux, try again with libunwind if we fail with the gimli based unwinder
        #[cfg(target_os="linux")]
        let native_stack = match self.get_thread(&thread) {
            Ok(x) => x,
            Err(_) =>  self.get_libunwind_thread(&thread)?
        };

        // TODO: merging the two stack together could happen outside of thread lock
        #[cfg(not(target_os="linux"))]
        return self.merge_native_stack(frames, native_stack);

        #[cfg(target_os="linux")]
        match self.merge_native_stack(frames, native_stack) {
            Ok(merged) => return Ok(merged),
            Err(_) => {
                let native_stack = self.get_libunwind_thread(&thread)?;
                return self.merge_native_stack(frames, native_stack);
            }
        }
    }

    pub fn merge_native_stack(&mut self, frames: &Vec<Frame>, native_stack: Vec<u64>) -> Result<Vec<Frame>, Error> {
        let mut python_frame_index = 0;
        let mut merged = Vec::new();

        for addr in native_stack {
            let is_python_addr = self.python.contains(addr) || self.libpython.as_ref().map_or(false, |m| m.contains(addr));

            self.unwinder.symbolicate(addr, !is_python_addr, &mut |frame| {
                let mut insert_native = true;

                if is_python_addr || frame.module == self.python.filename {
                    insert_native = false;
                    if let Some(ref function) = frame.function {
                        // ugh, probably could do a better job of figuring this out
                        // (also the symbols are different for each OS)
                        if function == "PyEval_EvalFrameDefault" ||
                            function == "_PyEval_EvalFrameDefault" ||
                            function == "__PyEval_EvalFrameDefault" ||
                            function == "PyEval_EvalFrameEx" {
                                            insert_native = false;

                            // if we have a corresponding python frame for the evalframe
                            // merge it into the stack. (if we're out of bounds a later
                            // check will pick up - and report overall totals mismatch)
                            if python_frame_index < frames.len() {
                                merged.push(frames[python_frame_index].clone());
                            }
                            python_frame_index += 1;
                        // Certain python functions are worth calling out, for visualizing things
                        // like GIL contention etc
                        } else if function == "_time_sleep" || function == "time_sleep" ||
                            function == "PyGILState_Ensure" || function == "_PyGILState_Ensure" {
                            insert_native = true;
                        }
                    }
                }

                if insert_native {
                    match &frame.function {
                        Some(func) =>  {
                            if ignore_frame(func, &frame.module) {
                                return;
                            }

                            // Get the filename/line/function name here
                            let line = frame.line.unwrap_or(0) as i32;

                            // try to resolve the filename relative to the module if given
                            let filename = match frame.filename.as_ref() {
                                Some(filename) => {
                                    resolve_filename(filename, &frame.module)
                                        .unwrap_or_else(|| filename.clone())
                                },
                                None => frame.module.clone()
                            };

                            let mut demangled = None;
                            if func.starts_with('_') {
                                if let Ok((sym, _)) = ::cpp_demangle::BorrowedSymbol::with_tail(func.as_bytes()) {
                                    let mut options = ::cpp_demangle::DemangleOptions::default();
                                    options.no_params = true;
                                    if let Ok(sym) = sym.demangle(&options) {
                                        demangled = Some(sym);
                                    }
                                }
                            }
                            let name = demangled.as_ref().unwrap_or_else(|| &func);
                            if cython::ignore_frame(name) {
                                return;
                            }
                            let name = cython::demangle(&name).to_owned();
                            merged.push(Frame{filename, line, name, short_filename: None, module: Some(frame.module.clone())})
                        },
                        None => {
                            merged.push(Frame{filename: frame.module.clone(),
                                              name: format!("0x{:X}", addr),
                                              line: 0, short_filename: None, module: Some(frame.module.clone())})
                        }
                    }
                }
            }).unwrap_or_else(|e| {
                if let remoteprocess::Error::NoBinaryForAddress(_) = e {
                    self.should_reload = true;
                }
                // if we can't symbolicate, just insert a stub here.
                merged.push(Frame{filename: "?".to_owned(),
                                  name: format!("0x{:X}", addr),
                                  line: 0, short_filename: None, module: None});
            });
        }

        if python_frame_index != frames.len() {
            return Err(format_err!("Failed to merge native and python frames (Have {} native and {} python",
                                    python_frame_index, frames.len()));
        }

        for frame in merged.iter_mut() {
            self.cython_maps.translate(frame);
        }

        Ok(merged)
    }


    fn get_thread(&mut self, thread: &remoteprocess::Thread) -> Result<Vec<u64>, Error> {
        let mut stack = Vec::new();
        let mut cursor = self.unwinder.cursor(thread)?;

        while let Some(ip) = cursor.next() {
            if let Err(remoteprocess::Error::NoBinaryForAddress(_)) = ip {
                // self.should_reload = true;
            }
            stack.push(ip?);
        }
        Ok(stack)
    }

    #[cfg(target_os="linux")]
    fn get_libunwind_thread(&self, thread: &remoteprocess::Thread) -> Result<Vec<u64>, Error> {
        let mut stack = Vec::new();
        for ip in self.libunwinder.cursor(thread.id()? as i32)? {
            stack.push(ip?);
        }
        Ok(stack)
    }

    #[cfg(target_os="linux")]
    pub fn get_pthread_id(&self, thread: &remoteprocess::Thread, threadids: &HashSet<u64>) -> Result<u64, Error> {
        let mut pthread_id = 0;

        let mut cursor = self.libunwinder.cursor(thread.id()? as i32)?;
        while let Some(_) = cursor.next() {
            // the pthread_id is usually in the top-level frame of the thread, but on some configs
            // can be 2nd level. Handle this by taking the top-most rbx value that is one of the
            // pthread_ids we're looking for
            if let Ok(bx) = cursor.bx() {
                if bx != 0 && threadids.contains(&bx) {
                    pthread_id = bx;
                }
            }
        }

        Ok(pthread_id)
    }
}

// the intent here is to remove top-level libc or pthreads calls
// from the stack traces. This almost certainly can be done better
#[cfg(target_os="linux")]
fn ignore_frame(function: &str, module: &str) -> bool {
    if function == "__libc_start_main" && module.contains("/libc") {
        return true;
    }

    if function == "__clone" && module.contains("/libc") {
        return true;
    }

    if function == "start_thread" && module.contains("/libpthread") {
        return true;
    }

    false
}

#[cfg(target_os="macos")]
fn ignore_frame(function: &str, module: &str) -> bool {
    if function == "_start" && module.contains("/libdyld.dylib") {
        return true;
    }

    if function == "__pthread_body" && module.contains("/libsystem_pthread") {
        return true;
    }

    if function == "_thread_start" && module.contains("/libsystem_pthread") {
        return true;
    }

    false
}

#[cfg(windows)]
fn ignore_frame(function: &str, module: &str) -> bool {
    if function == "RtlUserThreadStart" && module.to_lowercase().ends_with("ntdll.dll") {
        return true;
    }

    if function == "BaseThreadInitThunk" && module.to_lowercase().ends_with("kernel32.dll") {
        return true;
    }

    false
}
