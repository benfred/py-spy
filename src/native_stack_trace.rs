use anyhow::Error;
use std::collections::HashSet;
use std::num::NonZeroUsize;

use cpp_demangle::{BorrowedSymbol, DemangleOptions};
use lazy_static::lazy_static;
use lru::LruCache;
use remoteprocess::{self, Pid, Tid};

use crate::binary_parser::BinaryInfo;
use crate::cython;
use crate::stack_trace::{Frame, StackTrace};
use crate::utils::resolve_filename;

pub struct NativeStack {
    should_reload: bool,
    python: Option<BinaryInfo>,
    libpython: Option<BinaryInfo>,
    cython_maps: cython::SourceMaps,
    unwinder: remoteprocess::Unwinder,
    symbolicator: remoteprocess::Symbolicator,
    // TODO: right now on windows if we don't hold on the process handle unwinding will fail
    #[allow(dead_code)]
    process: remoteprocess::Process,
    symbol_cache: LruCache<u64, remoteprocess::StackFrame>,
}

impl NativeStack {
    pub fn new(
        pid: Pid,
        python: Option<BinaryInfo>,
        libpython: Option<BinaryInfo>,
    ) -> Result<NativeStack, Error> {
        let cython_maps = cython::SourceMaps::new();

        let process = remoteprocess::Process::new(pid)?;
        let unwinder = process.unwinder()?;
        let symbolicator = process.symbolicator()?;

        Ok(NativeStack {
            cython_maps,
            unwinder,
            symbolicator,
            should_reload: false,
            python,
            libpython,
            process,
            symbol_cache: LruCache::new(NonZeroUsize::new(65536).unwrap()),
        })
    }

    pub fn merge_native_thread(
        &mut self,
        frames: &Vec<Frame>,
        thread: &remoteprocess::Thread,
    ) -> Result<Vec<Frame>, Error> {
        if self.should_reload {
            self.symbolicator.reload()?;
            self.should_reload = false;
        }

        // get the native stack from the thread
        let native_stack = self.get_thread(thread)?;

        // TODO: merging the two stack together could happen outside of thread lock
        self.merge_native_stack(frames, native_stack)
    }
    pub fn merge_native_stack(
        &mut self,
        frames: &Vec<Frame>,
        native_stack: Vec<u64>,
    ) -> Result<Vec<Frame>, Error> {
        let mut python_frame_index = 0;
        let mut merged = Vec::new();

        // merge the native_stack and python stack together
        for addr in native_stack {
            // check in the symbol cache if we have looked up this symbol yet
            let cached_symbol = self.symbol_cache.get(&addr).cloned();

            // merges a remoteprocess::StackFrame into the current merged vec
            let is_python_addr = self.python.as_ref().map_or(false, |m| m.contains(addr))
                || self.libpython.as_ref().map_or(false, |m| m.contains(addr));
            let merge_frame = &mut |frame: &remoteprocess::StackFrame| {
                match self.get_merge_strategy(is_python_addr, frame) {
                    MergeType::Ignore => {}
                    MergeType::MergeNativeFrame => {
                        if let Some(python_frame) = self.translate_native_frame(frame) {
                            merged.push(python_frame);
                        }
                    }
                    MergeType::MergePythonFrame => {
                        // if we have a corresponding python frame for the evalframe
                        // merge it into the stack. (if we're out of bounds a later
                        // check will pick up - and report overall totals mismatch)

                        // Merge all python frames until we hit one with `is_entry`.
                        while python_frame_index < frames.len() {
                            merged.push(frames[python_frame_index].clone());

                            if frames[python_frame_index].is_entry {
                                break;
                            }

                            python_frame_index += 1;
                        }
                        python_frame_index += 1;
                    }
                }
            };

            if let Some(frame) = cached_symbol {
                merge_frame(&frame);
                continue;
            }

            // Keep track of the first symbolicated frame for caching. We don't cache anything (yet) where
            // symoblicationg returns multiple frames for an address, like in the case of inlined function calls.
            // so track how many frames we get for the address, and only update cache in the happy case
            // of 1 frame
            let mut symbolicated_count = 0;
            let mut first_frame = None;

            self.symbolicator
                .symbolicate(
                    addr,
                    !is_python_addr,
                    &mut |frame: &remoteprocess::StackFrame| {
                        symbolicated_count += 1;
                        if symbolicated_count == 1 {
                            first_frame = Some(frame.clone());
                        }
                        merge_frame(frame);
                    },
                )
                .unwrap_or_else(|e| {
                    if let remoteprocess::Error::NoBinaryForAddress(_) = e {
                        debug!(
                            "don't have a binary for symbols at 0x{:x} - reloading",
                            addr
                        );
                        self.should_reload = true;
                    }
                    // if we can't symbolicate, just insert a stub here.
                    merged.push(Frame {
                        filename: "?".to_owned(),
                        name: format!("0x{:x}", addr),
                        line: 0,
                        short_filename: None,
                        module: None,
                        locals: None,
                        is_entry: true,
                    });
                });

            if symbolicated_count == 1 {
                self.symbol_cache.put(addr, first_frame.unwrap());
            }
        }

        if python_frame_index != frames.len() {
            if python_frame_index == 0 {
                // I've seen a problem come up a bunch where we only get 1-2 native stack traces and then it fails
                // (with a valid python stack trace on top of that). both the gimli and libunwind unwinder don't
                // return the full stack, and connecting up to the process with GDB brings a corrupt stack error:
                //    from /home/ben/anaconda3/lib/python3.7/site-packages/numpy/core/../../../../libmkl_avx512.so
                //    Backtrace stopped: previous frame inner to this frame (corrupt stack?)
                //
                // rather than fail here, lets just insert the python frames after the native frames
                for frame in frames {
                    merged.push(frame.clone());
                }
            } else if python_frame_index == frames.len() + 1 {
                // if we have seen exactly one more python frame in the native stack than the python stack - let it go.
                // (can happen when the python stack has been unwound, but haven't exited the PyEvalFrame function
                // yet)
                info!(
                    "Have {} native and {} python threads in stack - allowing for now",
                    python_frame_index,
                    frames.len()
                );
            } else {
                return Err(format_err!(
                    "Failed to merge native and python frames (Have {} native and {} python)",
                    python_frame_index,
                    frames.len()
                ));
            }
        }

        // TODO: can this by merged into translate_frame?
        for frame in merged.iter_mut() {
            self.cython_maps.translate(frame);
        }

        Ok(merged)
    }

    fn get_merge_strategy(
        &self,
        check_python: bool,
        frame: &remoteprocess::StackFrame,
    ) -> MergeType {
        if check_python {
            if let Some(ref function) = frame.function {
                // We want to include some internal python functions. For example, calls like time.sleep
                // or os.kill etc are implemented as builtins in the interpreter and filtering them out
                // is misleading. Create a set of whitelisted python function prefixes to include
                lazy_static! {
                    static ref WHITELISTED_PREFIXES: HashSet<&'static str> = {
                        let mut prefixes = HashSet::new();
                        prefixes.insert("time");
                        prefixes.insert("sys");
                        prefixes.insert("gc");
                        prefixes.insert("os");
                        prefixes.insert("unicode");
                        prefixes.insert("thread");
                        prefixes.insert("stringio");
                        prefixes.insert("sre");
                        // likewise reasoning about lock contention inside python is also useful
                        prefixes.insert("PyGilState");
                        prefixes.insert("PyThread");
                        prefixes.insert("lock");
                        prefixes
                    };
                }

                // Figure out the merge type by looking at the function name, frames that
                // are used in evaluating python code are ignored, aside from PyEval_EvalFrame*
                // which is replaced by the function from the python stack
                // note: we're splitting on both _ and . to handle symbols like
                // _PyEval_EvalFrameDefault.cold.2962
                let mut tokens = function.split(&['_', '.'][..]).filter(|&x| !x.is_empty());
                match tokens.next() {
                    Some("PyEval") => match tokens.next() {
                        Some("EvalFrameDefault") => MergeType::MergePythonFrame,
                        Some("EvalFrameEx") => MergeType::MergePythonFrame,
                        _ => MergeType::Ignore,
                    },
                    Some(prefix) if WHITELISTED_PREFIXES.contains(prefix) => {
                        MergeType::MergeNativeFrame
                    }
                    _ => MergeType::Ignore,
                }
            } else {
                // is this correct? if we don't have a function name and in python binary should ignore?
                MergeType::Ignore
            }
        } else {
            MergeType::MergeNativeFrame
        }
    }

    pub fn add_native_only_threads(
        &mut self,
        process: &remoteprocess::Process,
        traces: &mut Vec<StackTrace>,
    ) -> Result<(), Error> {
        // Set of all threads we already processed
        let seen_threads =
            HashSet::<Tid>::from_iter(traces.iter().map(|t| t.os_thread_id.unwrap_or(0) as Tid));

        for native_thread in process.threads()?.into_iter() {
            let tid = native_thread.id()?;

            if seen_threads.contains(&tid) {
                // We've already seen this thread, don't add it again
                continue;
            }

            // We are reusing the `merge_native_stack` method and just pass an
            // empty python stack.
            let native_stack = self.get_thread(&native_thread)?;
            let python_stack = Vec::new();
            let symbolized_stack = self.merge_native_stack(&python_stack, native_stack)?;

            // Push new stack trace
            traces.push(StackTrace {
                pid: process.pid,
                thread_id: tid.try_into().unwrap_or(0),
                thread_name: None,
                os_thread_id: tid.try_into().ok(),
                active: native_thread.active().unwrap_or(false),
                owns_gil: false,
                frames: symbolized_stack,
                process_info: None,
            });
        }
        Ok(())
    }

    /// translates a native frame into a optional frame. none indicates we should ignore this frame
    fn translate_native_frame(&self, frame: &remoteprocess::StackFrame) -> Option<Frame> {
        match &frame.function {
            Some(func) => {
                if ignore_frame(func, &frame.module) {
                    return None;
                }

                // Get the filename/line/function name here
                let line = frame.line.unwrap_or(0) as i32;

                // try to resolve the filename relative to the module if given
                let filename = match frame.filename.as_ref() {
                    Some(filename) => resolve_filename(filename, &frame.module)
                        .unwrap_or_else(|| filename.clone()),
                    None => frame.module.clone(),
                };

                let mut demangled = None;
                if func.starts_with('_') {
                    if let Ok((sym, _)) = BorrowedSymbol::with_tail(func.as_bytes()) {
                        let options = DemangleOptions::new().no_params().no_return_type();
                        if let Ok(sym) = sym.demangle(&options) {
                            demangled = Some(sym);
                        }
                    }
                }
                let name = demangled.as_ref().unwrap_or(func);
                if cython::ignore_frame(name) {
                    return None;
                }
                let name = cython::demangle(name).to_owned();
                Some(Frame {
                    filename,
                    line,
                    name,
                    short_filename: None,
                    module: Some(frame.module.clone()),
                    locals: None,
                    is_entry: true,
                })
            }
            None => Some(Frame {
                filename: frame.module.clone(),
                name: format!("0x{:x}", frame.addr),
                locals: None,
                line: 0,
                short_filename: None,
                module: Some(frame.module.clone()),
                is_entry: true,
            }),
        }
    }

    fn get_thread(&mut self, thread: &remoteprocess::Thread) -> Result<Vec<u64>, Error> {
        let mut stack = Vec::new();
        for ip in self.unwinder.cursor(thread)? {
            stack.push(ip?);
        }
        Ok(stack)
    }
}

#[derive(Debug)]
enum MergeType {
    Ignore,
    MergePythonFrame,
    MergeNativeFrame,
}

// the intent here is to remove top-level libc or pthreads calls
// from the stack traces. This almost certainly can be done better
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "macos")]
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
