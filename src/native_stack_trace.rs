use std::collections::{HashMap, HashSet};

use failure::Error;

use remoteprocess::{self, ProcessMemory, Pid};

use crate::python_interpreters::{InterpreterState};
use crate::cython;
use crate::stack_trace::{Frame, StackTrace, get_stack_traces};
use crate::utils::resolve_filename;

pub struct NativeStack {
    should_reload: bool,
    process: remoteprocess::Process,
    python_filename: String,
    libpython_filename: Option<String>,
    cython_maps: cython::SourceMaps,
    unwinder: remoteprocess::Unwinder,
    // on linux, we also fallback to using libunwind if the main gimli based unwinder fails
    // (and libunwind is installed)
    #[cfg(target_os="linux")]
    libunwinder: Option<remoteprocess::LibUnwind>,
}

impl NativeStack {
    pub fn new(pid: Pid, python_filename: &str, libpython_filename: &Option<String>) -> Result<NativeStack, Error> {
        let cython_maps = cython::SourceMaps::new();

        let process = remoteprocess::Process::new(pid)?;
        let unwinder = process.unwinder()?;

        // Try to load up libunwind-ptrace on linux
        #[cfg(target_os="linux")]
        let libunwinder = {
            match remoteprocess::libunwind::LibUnwind::new() {
                Ok(libunwinder) => Some(libunwinder),
                Err(e) => {
                    warn!("Failed to load libunwind-ptrace: {:?}", e);
                    static mut SHOWN_WARNING: bool = false;
                    unsafe {
                        if !SHOWN_WARNING {
                            eprintln!("\nFailed to load libunwind-ptrace, you may have an elevated error rate");
                            eprintln!("You can install libunwind-ptrace on ubuntu by going 'sudo apt install libunwind-dev'.\n");
                            SHOWN_WARNING = true;
                        }
                    }

                    None
                }
             }
        };

        return Ok(NativeStack{process, cython_maps, unwinder, should_reload: false,
                              python_filename: python_filename.to_owned(),
                              libpython_filename: libpython_filename.clone(),
                              #[cfg(target_os="linux")]
                              libunwinder
                              });
    }

    /// Gets merged Python/Native stack traces
    pub fn get_native_stack_traces<I, P>(&mut self, interpreter: &I, process: &P) -> Result<(Vec<StackTrace>), Error>
            where I: InterpreterState, P: ProcessMemory {
        if self.should_reload {
            self.unwinder.reload()?;
            self.should_reload = false;
        }

        // Get the native stack trace for each thread in the process
        let mut native_stacks = HashMap::new();
        let mut threadid_map = HashMap::new();
        let mut traces;
        let mut threadids = HashSet::new();

        // get all the python stack traces and native stack traces here
        // (locking to get a consistent snapshot, but releasing the lock
        // before we merge the stack traces or symbolicate)
        {
            let _lock = self.process.lock()?;
            traces = get_stack_traces(interpreter, process)?;
            for trace in traces.iter() {
                threadids.insert(trace.thread_id);
            }

            for thread in self.process.threads()? {
                #[cfg(not(target_os="linux"))]
                let (stack, python_thread_id) = self.get_thread(&threadids, &thread)?;

                // on linux, try again with libunwind if we fail with the gimli based unwinder
                #[cfg(target_os="linux")]
                let (stack, python_thread_id) = match self.get_thread(&threadids, &thread) {
                    Ok(x) => x,
                    Err(e) => {
                        if self.libunwinder.is_some() {
                            self.get_libunwind_thread(&threadids, &thread)?
                        } else {
                            return Err(e);
                        }
                    }
                };

                native_stacks.insert(thread, stack);
                threadid_map.entry(python_thread_id).or_insert(thread);
            }
        }

        for trace in traces.iter_mut() {
            let os_thread_id = match threadid_map.get(&trace.thread_id) {
                Some(thread) => *thread,
                None => threadid_map[&0] // TODO: handle this
            };

            let stack = &native_stacks[&os_thread_id];
            let mut python_frame_index = 0;
            let mut merged = Vec::new();

            for addr in stack {
                self.unwinder.symbolicate(*addr, &mut |frame| {
                    // TODO: for figuring out python frames/functions don't symbolicate
                    // and instead just figure out if the addr is in the range
                    #[cfg(unix)]
                    let is_python_exe = frame.module == self.python_filename;

                    #[cfg(windows)]
                    let is_python_exe = frame.module.to_lowercase() == self.python_filename.to_lowercase();

                    let mut insert_native = true;

                    if is_python_exe ||
                       Some(&frame.module) == self.libpython_filename.as_ref() ||
                       self.python_filename.starts_with(&frame.module) {

                        insert_native = false;
                        if let Some(ref function) = frame.function {

                            // ugh, probably could do a better job of figuring this out
                            // (also the symbols are different for each OS)
                            if function == "PyEval_EvalFrameDefault" ||
                               function == "_PyEval_EvalFrameDefault" ||
                               function == "__PyEval_EvalFrameDefault" ||
                               function == "PyEval_EvalFrameEx" {

                                // if we have a corresponding python frame for the evalframe
                                // merge it into the stack. (if we're out of bounds a later
                                // check will pick up - and report overall totals mismatch)
                                if python_frame_index < trace.frames.len() {
                                    merged.push(trace.frames[python_frame_index].clone());
                                }
                                python_frame_index += 1;
                            } else if function == "_time_sleep" || function == "time_sleep" {
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
                                                  name: "?".to_owned(),
                                                  line: 0, short_filename: None, module: Some(frame.module.clone())})
                            }
                        }
                    }
                }).unwrap_or_else(|_| {
                    // if we can't symbolicate, just insert a stub here.
                    merged.push(Frame{filename: "?".to_owned(),
                                      name: "?".to_owned(),
                                      line: 0, short_filename: None, module: None});
                });
            }

            if python_frame_index != trace.frames.len() {
                // TODO: on linux in this case, fallback to libunwind. Vast majority of errors are here
                // this requires some refactoring here though (don't have thread lock here).
                // feel like we should only get lock one thread at a time when sampling - and move
                // code to match the pythonthreadid/os thread id out - and only load native stack when/as
                // needed
                return Err(format_err!("Failed to merge native and python frames (Have {} native and {} python",
                                       python_frame_index, trace.frames.len()));
            }

            for frame in merged.iter_mut() {
                self.cython_maps.translate(frame);
            }
            trace.os_thread_id = Some(os_thread_id.id()?);
            trace.frames = merged;
        }
        Ok(traces)
    }

    fn get_thread(&mut self, threadids: &HashSet<u64>, thread: &remoteprocess::Thread) -> Result<(Vec<u64>, u64), Error> {
        let mut stack = Vec::new();
        let mut cursor = self.unwinder.cursor(thread)?;
        #[allow(unused_assignments)]
        let mut python_thread_id = 0;

        while let Some(ip) = cursor.next() {
            if let Err(remoteprocess::Error::NoBinaryForAddress(_)) = ip {
                self.should_reload = true;
            }
            stack.push(ip?);

            // On unix based systems w/ pthreads - the python thread id
            // is contained in the RBX register of the last frame (aside from main frame)
            // This is sort of a massive hack, but seems to work
            #[cfg(unix)]
            {
                let next_bx = cursor.bx();
                if next_bx != 0 && threadids.contains(&next_bx)  {
                    python_thread_id = next_bx;
                }
            }
        }

        #[cfg(windows)]
        {
        python_thread_id = thread.id()?;
        }

        Ok((stack, python_thread_id))
    }

    #[cfg(target_os="linux")]
    fn get_libunwind_thread(&self, threadids: &HashSet<u64>, thread: &remoteprocess::Thread) -> Result<(Vec<u64>, u64), Error> {
        let mut stack = Vec::new();
        let unwinder = self.libunwinder.as_ref().unwrap();
        let mut cursor = unwinder.cursor(thread.id()? as i32)?;
        let mut bx = 0;
        while let Some(ip) = cursor.next() {
            if let Ok(next_bx) = cursor.bx() {
                if next_bx != 0 && threadids.contains(&next_bx)  {
                    bx = next_bx;
                }
            }
            stack.push(ip?);
        }

        Ok((stack, bx))
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
