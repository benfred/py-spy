use std;
use std::collections::HashMap;
#[cfg(all(target_os="linux", unwind))]
use std::collections::HashSet;
use std::mem::size_of;
use std::slice;
use std::path::Path;
#[cfg(all(target_os="linux", unwind))]
use std::iter::FromIterator;
use regex::Regex;
#[cfg(windows)]
use regex::RegexBuilder;

use anyhow::{Error, Result, Context};
use lazy_static::lazy_static;
use remoteprocess::{Process, ProcessMemory, Pid, Tid};
use proc_maps::{get_process_maps, MapRange};


use crate::binary_parser::{parse_binary, BinaryInfo};
use crate::config::{Config, LockingStrategy, LineNo};
#[cfg(unwind)]
use crate::native_stack_trace::NativeStack;
use crate::python_bindings::{pyruntime, v2_7_15, v3_3_7, v3_5_5, v3_6_6, v3_7_0, v3_8_0, v3_9_5, v3_10_0, v3_11_0};
use crate::python_interpreters::{self, InterpreterState, ThreadState};
use crate::python_threading::thread_name_lookup;
use crate::stack_trace::{StackTrace, get_stack_traces, get_stack_trace};
use crate::version::Version;

/// Lets you retrieve stack traces of a running python program
pub struct PythonSpy {
    pub pid: Pid,
    pub process: Process,
    pub version: Version,
    pub interpreter_address: usize,
    pub threadstate_address: usize,
    pub python_filename: std::path::PathBuf,
    pub version_string: String,
    pub config: Config,
    #[cfg(unwind)]
    pub native: Option<NativeStack>,
    pub short_filenames: HashMap<String, Option<String>>,
    pub python_thread_ids: HashMap<u64, Tid>,
    pub python_thread_names: HashMap<u64, String>,
    #[cfg(target_os="linux")]
    pub dockerized: bool
}

fn error_if_gil(config: &Config, version: &Version, msg: &str) -> Result<(), Error> {
    lazy_static! {
        static ref WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    }

    if config.gil_only {
        if !WARNED.load(std::sync::atomic::Ordering::Relaxed) {
            // only print this once
            eprintln!("Cannot detect GIL holding in version '{}' on the current platform (reason: {})", version, msg);
            eprintln!("Please open an issue in https://github.com/benfred/py-spy with the Python version and your platform.");
            WARNED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Err(format_err!("Cannot detect GIL holding in version '{}' on the current platform (reason: {})", version, msg))
    } else {
        warn!("Unable to detect GIL usage: {}", msg);
        Ok(())
    }
}

impl PythonSpy {
    /// Constructs a new PythonSpy object.
    pub fn new(pid: Pid, config: &Config) -> Result<PythonSpy, Error> {
        let process = remoteprocess::Process::new(pid)
            .context("Failed to open process - check if it is running.")?;

        // get basic process information (memory maps/symbols etc)
        let python_info = PythonProcessInfo::new(&process)?;

        // lock the process when loading up on freebsd (rather than locking
        // on every memory read). Needs done after getting python process info
        // because procmaps also tries to attach w/ ptrace on freebsd
        #[cfg(target_os="freebsd")]
        let _lock = process.lock();

        let version = get_python_version(&python_info, &process)?;
        info!("python version {} detected", version);

        let interpreter_address = get_interpreter_address(&python_info, &process, &version)?;
        info!("Found interpreter at 0x{:016x}", interpreter_address);

        // lets us figure out which thread has the GIL
         let threadstate_address = match version {
             Version{major: 3, minor: 7..=11, ..} => {
                match python_info.get_symbol("_PyRuntime") {
                    Some(&addr) => {
                        if let Some(offset) = pyruntime::get_tstate_current_offset(&version) {
                            info!("Found _PyRuntime @ 0x{:016x}, getting gilstate.tstate_current from offset 0x{:x}",
                                addr, offset);
                            addr as usize + offset
                        } else {
                            error_if_gil(config, &version, "unknown pyruntime.gilstate.tstate_current offset")?;
                            0
                        }
                    },
                    None => {
                        error_if_gil(config, &version, "failed to find _PyRuntime symbol")?;
                        0
                    }
                }
             },
             _ => {
                 match python_info.get_symbol("_PyThreadState_Current") {
                    Some(&addr) => {
                        info!("Found _PyThreadState_Current @ 0x{:016x}", addr);
                        addr as usize
                    },
                    None => {
                        error_if_gil(config, &version, "failed to find _PyThreadState_Current symbol")?;
                        0
                    }
                }
             }
         };

        let version_string = format!("python{}.{}", version.major, version.minor);

        #[cfg(unwind)]
        let native = if config.native {
            Some(NativeStack::new(pid, python_info.python_binary, python_info.libpython_binary)?)
        } else {
            None
        };

        Ok(PythonSpy{pid, process, version, interpreter_address, threadstate_address,
                     python_filename: python_info.python_filename,
                     version_string,
                     #[cfg(unwind)]
                     native,
                     #[cfg(target_os="linux")]
                     dockerized: python_info.dockerized,
                     config: config.clone(),
                     short_filenames: HashMap::new(),
                     python_thread_ids: HashMap::new(),
                     python_thread_names: HashMap::new()})
    }

    /// Creates a PythonSpy object, retrying up to max_retries times.
    /// Mainly useful for the case where the process is just started and
    /// symbols or the python interpreter might not be loaded yet.
    pub fn retry_new(pid: Pid, config: &Config, max_retries:u64) -> Result<PythonSpy, Error> {
        let mut retries = 0;
        loop {
            let err = match PythonSpy::new(pid, config) {
                Ok(mut process) => {
                    // verify that we can load a stack trace before returning success
                    match process.get_stack_traces() {
                        Ok(_) => return Ok(process),
                        Err(err) => err
                    }
                },
                Err(err) => err
            };

            // If we failed, retry a couple times before returning the last error
            retries += 1;
            if retries >= max_retries {
                return Err(err);
            }
            info!("Failed to connect to process, retrying. Error: {}", err);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Gets a StackTrace for each thread in the current process
    pub fn get_stack_traces(&mut self) -> Result<Vec<StackTrace>, Error> {
        match self.version {
            // ABI for 2.3/2.4/2.5/2.6/2.7 is compatible for our purpose
            Version{major: 2, minor: 3..=7, ..} => self._get_stack_traces::<v2_7_15::_is>(),
            Version{major: 3, minor: 3, ..} => self._get_stack_traces::<v3_3_7::_is>(),
            // ABI for 3.4 and 3.5 is the same for our purposes
            Version{major: 3, minor: 4, ..} => self._get_stack_traces::<v3_5_5::_is>(),
            Version{major: 3, minor: 5, ..} => self._get_stack_traces::<v3_5_5::_is>(),
            Version{major: 3, minor: 6, ..} => self._get_stack_traces::<v3_6_6::_is>(),
            Version{major: 3, minor: 7, ..} => self._get_stack_traces::<v3_7_0::_is>(),
            // v3.8.0a1 to v3.8.0a3 is compatible with 3.7 ABI, but later versions of 3.8.0 aren't
            Version{major: 3, minor: 8, patch: 0, ..} => {
                match self.version.release_flags.as_ref() {
                    "a1" | "a2" | "a3" => self._get_stack_traces::<v3_7_0::_is>(),
                    _ => self._get_stack_traces::<v3_8_0::_is>()
                }
            }
            Version{major: 3, minor: 8, ..} => self._get_stack_traces::<v3_8_0::_is>(),
            Version{major: 3, minor: 9, ..} => self._get_stack_traces::<v3_9_5::_is>(),
            Version{major: 3, minor: 10, ..} => self._get_stack_traces::<v3_10_0::_is>(),
            Version{major: 3, minor: 11, ..} => self._get_stack_traces::<v3_11_0::_is>(),
            _ => Err(format_err!("Unsupported version of Python: {}", self.version)),
        }
    }

    // implementation of get_stack_traces, where we have a type for the InterpreterState
    fn _get_stack_traces<I: InterpreterState>(&mut self) -> Result<Vec<StackTrace>, Error> {
        // Query the OS to get if each thread in the process is running or not
        let mut thread_activity = HashMap::new();
        for thread in self.process.threads()?.iter() {
            let threadid: Tid = thread.id()?;
            thread_activity.insert(threadid, thread.active()?);
        }

        // Lock the process if appropriate. Note we have to lock AFTER getting the thread
        // activity status from the OS (otherwise each thread would report being inactive always).
        // This has the potential for race conditions (in that the thread activity could change
        // between getting the status and locking the thread, but seems unavoidable right now
        let _lock = if self.config.blocking == LockingStrategy::Lock {
            Some(self.process.lock().context("Failed to suspend process")?)
        } else {
            None
        };

        let gil_thread_id = self._get_gil_threadid::<I>()?;

        // Get the python interpreter, and loop over all the python threads
        let interp: I = self.process.copy_struct(self.interpreter_address)
           .context("Failed to copy PyInterpreterState from process")?;

        let mut traces = Vec::new();
        let mut threads = interp.head();
        while !threads.is_null() {
            // Get the stack trace of the python thread
            let thread = self.process.copy_pointer(threads).context("Failed to copy PyThreadState")?;
            let mut trace = get_stack_trace(&thread, &self.process, self.config.dump_locals > 0, self.config.lineno)?;

            // Try getting the native thread id
            let python_thread_id = thread.thread_id();

            // python 3.11+ has the native thread id directly on the PyThreadState object,
            // so use that if available
            trace.os_thread_id = thread.native_thread_id();

            // for older versions of python, try using OS specific code to get the native
            // thread id (doesn' work on freebsd, or on arm/i686 processors on linux)
            if trace.os_thread_id.is_none() {
                let mut os_thread_id = self._get_os_thread_id(python_thread_id, &interp)?;

                // linux can see issues where pthread_ids get recycled for new OS threads,
                // which totally breaks the caching we were doing here. Detect this and retry
                if let Some(tid) = os_thread_id {
                    if thread_activity.len() > 0 && !thread_activity.contains_key(&tid) {
                        info!("clearing away thread id caches, thread {} has exited", tid);
                        self.python_thread_ids.clear();
                        self.python_thread_names.clear();
                        os_thread_id = self._get_os_thread_id(python_thread_id, &interp)?;
                    }
                }

                trace.os_thread_id = os_thread_id.map(|id| id as u64);
            }

            trace.thread_name = self._get_python_thread_name(python_thread_id);
            trace.owns_gil = trace.thread_id == gil_thread_id;

            // Figure out if the thread is sleeping from the OS if possible
            trace.active = true;
            if let Some(id) = trace.os_thread_id {
                let id = id as Tid;
                if let Some(active) = thread_activity.get(&id as _) {
                    trace.active = *active;
                }
            }

            // fallback to using a heuristic if we think the thread is still active
            // Note that on linux the OS thread activity can only be gotten on x86_64
            // processors and even then seems to be wrong occasionally in thinking 'select'
            // calls are active (which seems related to the thread locking code,
            // this problem doesn't seem to happen with the --nonblocking option)
            // Note: this should be done before the native merging for correct results
            if trace.active {
                trace.active = !self._heuristic_is_thread_idle(&trace);
            }

            // Merge in the native stack frames if necessary
            #[cfg(unwind)]
            {
                if self.config.native {
                    if let Some(native) = self.native.as_mut() {
                        let thread_id = trace.os_thread_id.ok_or_else(|| format_err!("failed to get os threadid"))?;
                        let os_thread = remoteprocess::Thread::new(thread_id as Tid)?;
                        trace.frames = native.merge_native_thread(&trace.frames, &os_thread)?
                    }
                }
            }

            for frame in &mut trace.frames {
                frame.short_filename = self.shorten_filename(&frame.filename);
                if let Some(locals) = frame.locals.as_mut() {
                    use crate::python_data_access::format_variable;
                    let max_length = (128 * self.config.dump_locals) as isize;
                    for local in locals {
                        let repr = format_variable::<I>(&self.process, &self.version, local.addr, max_length);
                        local.repr = Some(repr.unwrap_or("?".to_owned()));
                    }
                }
            }

            traces.push(trace);

            // This seems to happen occasionally when scanning BSS addresses for valid interpreters
            if traces.len() > 4096 {
                return Err(format_err!("Max thread recursion depth reached"));
            }

            threads = thread.next();
        }
        Ok(traces)
    }

    // heuristic fallback for determining if a thread is active, used
    // when we don't have the ability to get the thread information from the OS
    fn _heuristic_is_thread_idle(&self, trace: &StackTrace) -> bool {
        let frames = &trace.frames;
        if frames.is_empty() {
            // we could have 0 python frames, but still be active running native
            // code.
            false
        } else {
            let frame = &frames[0];
            (frame.name == "wait" && frame.filename.ends_with("threading.py")) ||
            (frame.name == "select" && frame.filename.ends_with("selectors.py")) ||
            (frame.name == "poll" && (frame.filename.ends_with("asyncore.py") ||
                                    frame.filename.contains("zmq") ||
                                    frame.filename.contains("gevent") ||
                                    frame.filename.contains("tornado")))
        }
    }

    #[cfg(windows)]
    fn _get_os_thread_id<I: InterpreterState>(&mut self, python_thread_id: u64, _interp: &I) -> Result<Option<Tid>, Error> {
        Ok(Some(python_thread_id as Tid))
    }

    #[cfg(target_os="macos")]
    fn _get_os_thread_id<I: InterpreterState>(&mut self, python_thread_id: u64, _interp: &I) -> Result<Option<Tid>, Error> {
        // If we've already know this threadid, we're good
        if let Some(thread_id) = self.python_thread_ids.get(&python_thread_id) {
            return Ok(Some(*thread_id));
        }

        for thread in self.process.threads()?.iter() {
            // ok, this is crazy pants. is this 224 constant right?  Is this right for all versions of OSX? how is this determined?
            // is this correct for all versions of python? Why does this even work?
            let current_handle = thread.thread_handle()? - 224;
            self.python_thread_ids.insert(current_handle, thread.id()?);
        }

        if let Some(thread_id) = self.python_thread_ids.get(&python_thread_id) {
            return Ok(Some(*thread_id));
        }
        Ok(None)
    }

    #[cfg(all(target_os="linux", not(unwind)))]
    fn _get_os_thread_id<I: InterpreterState>(&mut self, _python_thread_id: u64, _interp: &I) -> Result<Option<Tid>, Error> {
        Ok(None)
    }

    #[cfg(all(target_os="linux", unwind))]
    fn _get_os_thread_id<I: InterpreterState>(&mut self, python_thread_id: u64, interp: &I) -> Result<Option<Tid>, Error> {
        // in nonblocking mode, we can't get the threadid reliably (method here requires reading the RBX
        // register which requires a ptrace attach). fallback to heuristic thread activity here
        if self.config.blocking == LockingStrategy::NonBlocking {
            return Ok(None);
        }

        // likewise this doesn't yet work for profiling processes running inside docker containers from the host os
        if self.dockerized {
            return Ok(None);
        }

        // If we've already know this threadid, we're good
        if let Some(thread_id) = self.python_thread_ids.get(&python_thread_id) {
            return Ok(Some(*thread_id));
        }

        // Get a list of all the python thread ids
        let mut all_python_threads = HashSet::new();
        let mut threads = interp.head();
        while !threads.is_null() {
            let thread = self.process.copy_pointer(threads).context("Failed to copy PyThreadState")?;
            let current = thread.thread_id();
            all_python_threads.insert(current);
            threads = thread.next();
        }

        let processed_os_threads: HashSet<Tid> = HashSet::from_iter(self.python_thread_ids.values().map(|x| *x));

        let unwinder = self.process.unwinder()?;

        // Try getting the pthread_id from the native stack registers for threads we haven't looked up yet
        for thread in self.process.threads()?.iter() {
            let threadid = thread.id()?;
            if processed_os_threads.contains(&threadid) {
                continue;
            }

            match self._get_pthread_id(&unwinder, &thread, &all_python_threads) {
                Ok(pthread_id) => {
                    if pthread_id != 0 {
                        self.python_thread_ids.insert(pthread_id, threadid);
                    }
                },
                Err(e) => { warn!("Failed to get get_pthread_id for {}: {}", threadid, e); }
            };
        }

        // we can't get the python threadid for the main thread from registers,
        // so instead assign the main threadid (pid) to the missing python thread
        if !processed_os_threads.contains(&self.pid) {
            let mut unknown_python_threadids = HashSet::new();
            for python_thread_id in all_python_threads.iter() {
                if !self.python_thread_ids.contains_key(python_thread_id) {
                    unknown_python_threadids.insert(*python_thread_id);
                }
            }

            if unknown_python_threadids.len() == 1 {
                let python_thread_id = *unknown_python_threadids.iter().next().unwrap();
                self.python_thread_ids.insert(python_thread_id, self.pid);
            } else {
                warn!("failed to get python threadid for main thread!");
            }
        }

        if let Some(thread_id) = self.python_thread_ids.get(&python_thread_id) {
            return Ok(Some(*thread_id));
        }
        info!("failed looking up python threadid for {}. known python_thread_ids {:?}. all_python_threads {:?}",
            python_thread_id, self.python_thread_ids, all_python_threads);
        Ok(None)
    }


    #[cfg(all(target_os="linux", unwind))]
    pub fn _get_pthread_id(&self, unwinder: &remoteprocess::Unwinder, thread: &remoteprocess::Thread, threadids: &HashSet<u64>) -> Result<u64, Error> {
        let mut pthread_id = 0;

        let mut cursor = unwinder.cursor(thread)?;
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

    #[cfg(target_os="freebsd")]
    fn _get_os_thread_id<I: InterpreterState>(&mut self, _python_thread_id: u64, _interp: &I) -> Result<Option<Tid>, Error> {
        Ok(None)
    }

    fn _get_gil_threadid<I: InterpreterState>(&self) -> Result<u64, Error> {
        // figure out what thread has the GIL by inspecting _PyThreadState_Current
        if self.threadstate_address > 0 {
            let addr: usize = self.process.copy_struct(self.threadstate_address)?;

            // if the addr is 0, no thread is currently holding the GIL
            if addr != 0 {
                let threadstate: I::ThreadState = self.process.copy_struct(addr)?;
                return Ok(threadstate.thread_id());
            }
        }
        Ok(0)
    }

    fn _get_python_thread_name(&mut self, python_thread_id: u64) -> Option<String> {
        match self.python_thread_names.get(&python_thread_id) {
            Some(thread_name) => Some(thread_name.clone()),
            None => {
                self.python_thread_names = thread_name_lookup(self).unwrap_or_else(|| HashMap::new());
                self.python_thread_names.get(&python_thread_id).map(|name| name.clone())
            }
        }
    }

    /// We want to display filenames without the boilerplate of the python installation
    /// directory etc. This function looks only includes paths inside a python
    /// package or subpackage, and not the path the package is installed at
    fn shorten_filename(&mut self, filename: &str) -> Option<String> {
        // if the user requested full filenames, skip shortening
        if self.config.full_filenames {
            return Some(filename.to_string());
        }

        // if we have figured out the short filename already, use it
        if let Some(short) = self.short_filenames.get(filename) {
            return short.clone();
        }

        // on linux the process could be running in docker, access the filename through procfs
        #[cfg(target_os="linux")]
        let filename_storage;

        #[cfg(target_os="linux")]
        let filename = if self.dockerized {
            filename_storage = format!("/proc/{}/root{}", self.pid, filename);
            if Path::new(&filename_storage).exists() {
                &filename_storage
            } else {
                filename
            }
        } else {
            filename
        };

        // only include paths that include an __init__.py
        let mut path = Path::new(filename);
        while let Some(parent) = path.parent() {
            path = parent;
            if !parent.join("__init__.py").exists() {
                break;
            }
        }

        // remote the parent prefix and convert to an optional string
        let shortened = Path::new(filename)
            .strip_prefix(path)
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        self.short_filenames.insert(filename.to_owned(), shortened.clone());
        shortened
    }
}
/// Returns the version of python running in the process.
fn get_python_version(python_info: &PythonProcessInfo, process: &remoteprocess::Process)
        -> Result<Version, Error> {
    // If possible, grab the sys.version string from the processes memory (mac osx).
    if let Some(&addr) = python_info.get_symbol("Py_GetVersion.version") {
        info!("Getting version from symbol address");
        if let Ok(bytes) = process.copy(addr as usize, 128) {
            if let Ok(version) = Version::scan_bytes(&bytes) {
                return Ok(version);
            }
        }
    }

    // otherwise get version info from scanning BSS section for sys.version string
    if let Some(ref pb) = python_info.python_binary {
        info!("Getting version from python binary BSS");
        let bss = process.copy(pb.bss_addr as usize,
                               pb.bss_size as usize)?;
        match Version::scan_bytes(&bss) {
            Ok(version) => return Ok(version),
            Err(err) => info!("Failed to get version from BSS section: {}", err)
        }
    }

    // try again if there is a libpython.so
    if let Some(ref libpython) = python_info.libpython_binary {
        info!("Getting version from libpython BSS");
        let bss = process.copy(libpython.bss_addr as usize,
                               libpython.bss_size as usize)?;
        match Version::scan_bytes(&bss) {
            Ok(version) => return Ok(version),
            Err(err) => info!("Failed to get version from libpython BSS section: {}", err)
        }
    }

    // the python_filename might have the version encoded in it (/usr/bin/python3.5 etc).
    // try reading that in (will miss patch level on python, but that shouldn't matter)
    info!("Trying to get version from path: {}", python_info.python_filename.display());
    let path = Path::new(&python_info.python_filename);
    if let Some(python) = path.file_name() {
        if let Some(python) = python.to_str() {
            if python.starts_with("python") {
                let tokens: Vec<&str> = python[6..].split('.').collect();
                if tokens.len() >= 2 {
                    if let (Ok(major), Ok(minor)) = (tokens[0].parse::<u64>(), tokens[1].parse::<u64>()) {
                        return Ok(Version{major, minor, patch:0, release_flags: "".to_owned()})
                    }
                }
            }
        }
    }
    Err(format_err!("Failed to find python version from target process"))
}

fn get_interpreter_address(python_info: &PythonProcessInfo,
                           process: &remoteprocess::Process,
                           version: &Version) -> Result<usize, Error> {
    // get the address of the main PyInterpreterState object from loaded symbols if we can
    // (this tends to be faster than scanning through the bss section)
    match version {
        Version{major: 3, minor: 7..=11, ..} => {
            if let Some(&addr) = python_info.get_symbol("_PyRuntime") {
                let addr = process.copy_struct(addr as usize + pyruntime::get_interp_head_offset(&version))?;

                // Make sure the interpreter addr is valid before returning
                match check_interpreter_addresses(&[addr], &python_info.maps, process, version) {
                    Ok(addr) => return Ok(addr),
                    Err(_) => { warn!("Interpreter address from _PyRuntime symbol is invalid {:016x}", addr); }
                };
            }
        },
        _ => {
            if let Some(&addr) = python_info.get_symbol("interp_head") {
                let addr = process.copy_struct(addr as usize)?;
                match check_interpreter_addresses(&[addr], &python_info.maps, process, version) {
                    Ok(addr) => return Ok(addr),
                    Err(_) => { warn!("Interpreter address from interp_head symbol is invalid {:016x}", addr); }
                };
            }
        }
    };
    info!("Failed to get interp_head from symbols, scanning BSS section from main binary");

    // try scanning the BSS section of the binary for things that might be the interpreterstate
    let err =
        if let Some(ref pb) = python_info.python_binary {
            match get_interpreter_address_from_binary(pb, &python_info.maps, process, version) {
                Ok(addr) => return Ok(addr),
                err => Some(err)
            }
        } else {
            None
        };
    // Before giving up, try again if there is a libpython.so
    if let Some(ref lpb) = python_info.libpython_binary {
        info!("Failed to get interpreter from binary BSS, scanning libpython BSS");
        match get_interpreter_address_from_binary(lpb, &python_info.maps, process, version) {
            Ok(addr) => return Ok(addr),
            lib_err => err.unwrap_or(lib_err)
        }
    } else {
        err.expect("Both python and libpython are invalid.")
    }
}

fn get_interpreter_address_from_binary(binary: &BinaryInfo,
                                       maps: &[MapRange],
                                       process: &remoteprocess::Process,
                                       version: &Version) -> Result<usize, Error> {
    // We're going to scan the BSS/data section for things, and try to narrowly scan things that
    // look like pointers to PyinterpreterState
    let bss = process.copy(binary.bss_addr as usize, binary.bss_size as usize)?;

    #[allow(clippy::cast_ptr_alignment)]
    let addrs = unsafe { slice::from_raw_parts(bss.as_ptr() as *const usize, bss.len() / size_of::<usize>()) };
    check_interpreter_addresses(addrs, maps, process, version)
}

// Checks whether a block of memory (from BSS/.data etc) contains pointers that are pointing
// to a valid PyInterpreterState
fn check_interpreter_addresses(addrs: &[usize],
                               maps: &[MapRange],
                               process: &remoteprocess::Process,
                               version: &Version) -> Result<usize, Error> {
    // On windows, we can't just check if a pointer is valid by looking to see if it points
    // to something in the virtual memory map. Brute-force it instead
    #[cfg(windows)]
    fn maps_contain_addr(_: usize, _: &[MapRange]) -> bool { true }

    #[cfg(not(windows))]
    use proc_maps::maps_contain_addr;

    // This function does all the work, but needs a type of the interpreter
    fn check<I>(addrs: &[usize],
                maps: &[MapRange],
                process: &remoteprocess::Process) -> Result<usize, Error>
            where I: python_interpreters::InterpreterState {
        for &addr in addrs {
            if maps_contain_addr(addr, maps) {
                // this address points to valid memory. try loading it up as a PyInterpreterState
                // to further check
                let interp: I = match process.copy_struct(addr) {
                    Ok(interp) => interp,
                    Err(_) => continue
                };

                // get the pythreadstate pointer from the interpreter object, and if it is also
                // a valid pointer then load it up.
                let threads = interp.head();
                if maps_contain_addr(threads as usize, maps) {
                    // If the threadstate points back to the interpreter like we expect, then
                    // this is almost certainly the address of the intrepreter
                    let thread = match process.copy_pointer(threads) {
                        Ok(thread) => thread,
                        Err(_) => continue
                    };

                    // as a final sanity check, try getting the stack_traces, and only return if this works
                    if thread.interp() as usize == addr && get_stack_traces(&interp, process, LineNo::NoLine).is_ok() {
                        return Ok(addr);
                    }
                }
            }
        }
        Err(format_err!("Failed to find a python interpreter in the .data section"))
    }

    // different versions have different layouts, check as appropriate
    match version {
        Version{major: 2, minor: 3..=7, ..} => check::<v2_7_15::_is>(addrs, maps, process),
        Version{major: 3, minor: 3, ..} => check::<v3_3_7::_is>(addrs, maps, process),
        Version{major: 3, minor: 4..=5, ..} => check::<v3_5_5::_is>(addrs, maps, process),
        Version{major: 3, minor: 6, ..} => check::<v3_6_6::_is>(addrs, maps, process),
        Version{major: 3, minor: 7, ..} => check::<v3_7_0::_is>(addrs, maps, process),
        Version{major: 3, minor: 8, patch: 0, ..} => {
            match version.release_flags.as_ref() {
                "a1" | "a2" | "a3" => check::<v3_7_0::_is>(addrs, maps, process),
                _ => check::<v3_8_0::_is>(addrs, maps, process)
            }
        },
        Version{major: 3, minor: 8, ..} => check::<v3_8_0::_is>(addrs, maps, process),
        Version{major: 3, minor: 9, ..} => check::<v3_9_5::_is>(addrs, maps, process),
        Version{major: 3, minor: 10, ..} => check::<v3_10_0::_is>(addrs, maps, process),
        Version{major: 3, minor: 11, ..} => check::<v3_11_0::_is>(addrs, maps, process),
        _ => Err(format_err!("Unsupported version of Python: {}", version))
    }
}

/// Holds information about the python process: memory map layout, parsed binary info
/// for python /libpython etc.
pub struct PythonProcessInfo {
    python_binary: Option<BinaryInfo>,
    // if python was compiled with './configure --enabled-shared', code/symbols will
    // be in a libpython.so file instead of the executable. support that.
    libpython_binary: Option<BinaryInfo>,
    maps: Vec<MapRange>,
    python_filename: std::path::PathBuf,
    #[cfg(target_os="linux")]
    dockerized: bool,
}

impl PythonProcessInfo {
    fn new(process: &remoteprocess::Process) -> Result<PythonProcessInfo, Error> {
        let filename = process.exe()
            .context("Failed to get process executable name. Check that the process is running.")?;

        #[cfg(windows)]
        let filename = filename.to_lowercase();

        #[cfg(windows)]
        let is_python_bin = |pathname: &str| pathname.to_lowercase() == filename;

        #[cfg(not(windows))]
        let is_python_bin = |pathname: &str| pathname == filename;

        // get virtual memory layout
        let maps = get_process_maps(process.pid)?;
        info!("Got virtual memory maps from pid {}:", process.pid);
        for map in &maps {
            debug!("map: {:016x}-{:016x} {}{}{} {}", map.start(), map.start() + map.size(),
                if map.is_read() {'r'} else {'-'}, if map.is_write() {'w'} else {'-'}, if map.is_exec() {'x'} else {'-'},
                map.filename().unwrap_or(&std::path::PathBuf::from("")).display());
        }

        // parse the main python binary
        let (python_binary, python_filename) = {
            // Get the memory address for the executable by matching against virtual memory maps
            let map = maps.iter()
                .find(|m| {
                    if let Some(pathname) = m.filename() {
                        if let Some(pathname) = pathname.to_str() {
                            return is_python_bin(pathname) && m.is_exec();
                        }
                    }
                    false
                });

            let map = match map {
                Some(map) => map,
                None => {
                    warn!("Failed to find '{}' in virtual memory maps, falling back to first map region", filename);
                    // If we failed to find the executable in the virtual memory maps, just take the first file we find
                    // sometimes on windows get_process_exe returns stale info =( https://github.com/benfred/py-spy/issues/40
                    // and on all operating systems I've tried, the exe is the first region in the maps
                    &maps.first().ok_or_else(|| format_err!("Failed to get virtual memory maps from process"))?
                }
            };

            let filename = std::path::PathBuf::from(filename);

            // TODO: consistent types? u64 -> usize? for map.start etc
            #[allow(unused_mut)]
            let python_binary = parse_binary(process.pid, &filename, map.start() as u64, map.size() as u64, true)
                .and_then(|mut pb| {
                    // windows symbols are stored in separate files (.pdb), load
                    #[cfg(windows)]
                    {
                        get_windows_python_symbols(process.pid, &filename, map.start() as u64)
                            .map(|symbols| { pb.symbols.extend(symbols); pb })
                            .map_err(|err| err.into())
                    }

                    // For OSX, need to adjust main binary symbols by subtracting _mh_execute_header
                    // (which we've added to by map.start already, so undo that here)
                    #[cfg(target_os = "macos")]
                    {
                        let offset = pb.symbols["_mh_execute_header"] - map.start() as u64;
                        for address in pb.symbols.values_mut() {
                            *address -= offset;
                        }

                        if pb.bss_addr != 0 {
                            pb.bss_addr -= offset;
                        }
                    }

                    #[cfg(not(windows))]
                    Ok(pb)
                });

            (python_binary, filename.clone())
        };

        // likewise handle libpython for python versions compiled with --enabled-shared
        let libpython_binary = {
            let libmap = maps.iter()
                .find(|m| {
                    if let Some(pathname) = m.filename() {
                        if let Some(pathname) = pathname.to_str() {
                            return is_python_lib(pathname) && m.is_exec();
                        }
                    }
                    false
                });

            let mut libpython_binary: Option<BinaryInfo> = None;
            if let Some(libpython) = libmap {
                if let Some(filename) = &libpython.filename() {
                    info!("Found libpython binary @ {}", filename.display());
                    #[allow(unused_mut)]
                    let mut parsed = parse_binary(process.pid, filename, libpython.start() as u64, libpython.size() as u64, false)?;
                    #[cfg(windows)]
                    parsed.symbols.extend(get_windows_python_symbols(process.pid, filename, libpython.start() as u64)?);
                    libpython_binary = Some(parsed);
                }
            }

            // On OSX, it's possible that the Python library is a dylib loaded up from the system
            // framework (like /System/Library/Frameworks/Python.framework/Versions/2.7/Python)
            // In this case read in the dyld_info information and figure out the filename from there
            #[cfg(target_os = "macos")]
            {
                if libpython_binary.is_none() {
                    use proc_maps::mac_maps::get_dyld_info;
                    let dyld_infos = get_dyld_info(process.pid)?;

                    for dyld in &dyld_infos {
                        let segname = unsafe { std::ffi::CStr::from_ptr(dyld.segment.segname.as_ptr()) };
                        debug!("dyld: {:016x}-{:016x} {:10} {}",
                            dyld.segment.vmaddr, dyld.segment.vmaddr + dyld.segment.vmsize,
                            segname.to_string_lossy(), dyld.filename.display());
                    }

                    let python_dyld_data = dyld_infos.iter()
                        .find(|m| {
                            if let Some(filename) = m.filename.to_str() {
                                return is_python_framework(filename) &&
                                      m.segment.segname[0..7] == [95, 95, 68, 65, 84, 65, 0];
                            }
                            false
                        });


                    if let Some(libpython) = python_dyld_data {
                        info!("Found libpython binary from dyld @ {}", libpython.filename.display());

                        let mut binary = parse_binary(process.pid, &libpython.filename, libpython.segment.vmaddr, libpython.segment.vmsize, false)?;

                        // TODO: bss addr offsets returned from parsing binary are wrong
                        // (assumes data section isn't split from text section like done here).
                        // BSS occurs somewhere in the data section, just scan that
                        // (could later tighten this up to look at segment sections too)
                        binary.bss_addr = libpython.segment.vmaddr;
                        binary.bss_size = libpython.segment.vmsize;
                        libpython_binary = Some(binary);
                    }
                }
            }

            libpython_binary
        };

        // If we have a libpython binary - we can tolerate failures on parsing the main python binary.
        let python_binary = match libpython_binary {
            None => Some(python_binary.context("Failed to parse python binary")?),
            _ => python_binary.ok(),
        };

        #[cfg(target_os="linux")]
        let dockerized = is_dockerized(process.pid).unwrap_or(false);

        Ok(PythonProcessInfo{python_binary, libpython_binary, maps, python_filename,
                             #[cfg(target_os="linux")]
                             dockerized
        })
    }

    pub fn get_symbol(&self, symbol: &str) -> Option<&u64> {
        if let Some(ref pb) = self.python_binary {
            if let Some(addr) = pb.symbols.get(symbol) {
                info!("got symbol {} (0x{:016x}) from python binary", symbol, addr);
                return Some(addr);
            }
        }

        if let Some(ref binary) = self.libpython_binary {
            if let Some(addr) = binary.symbols.get(symbol) {
                info!("got symbol {} (0x{:016x}) from libpython binary", symbol, addr);
                return Some(addr);
            }
        }
        None
    }
}

#[cfg(target_os="linux")]
fn is_dockerized(pid: Pid) -> Result<bool, Error> {
    let self_mnt = std::fs::read_link("/proc/self/ns/mnt")?;
    let target_mnt = std::fs::read_link(&format!("/proc/{}/ns/mnt", pid))?;
    Ok(self_mnt != target_mnt)
}

// We can't use goblin to parse external symbol files (like in a separate .pdb file) on windows,
// So use the win32 api to load up the couple of symbols we need on windows. Note:
// we still can get export's from the PE file
#[cfg(windows)]
pub fn get_windows_python_symbols(pid: Pid, filename: &Path, offset: u64) -> std::io::Result<HashMap<String, u64>> {
    use proc_maps::win_maps::SymbolLoader;

    let handler = SymbolLoader::new(pid)?;
    let _module = handler.load_module(filename)?; // need to keep this module in scope

    let mut ret = HashMap::new();

    // currently we only need a subset of symbols, and enumerating the symbols is
    // expensive (via SymEnumSymbolsW), so rather than load up all symbols like we
    // do for goblin, just load the the couple we need directly.
    for symbol in ["_PyThreadState_Current", "interp_head", "_PyRuntime"].iter() {
        if let Ok((base, addr)) = handler.address_from_name(symbol) {
            // If we have a module base (ie from PDB), need to adjust by the offset
            // otherwise seems like we can take address directly
            let addr = if base == 0 { addr } else { offset + addr - base };
            ret.insert(String::from(*symbol), addr);
        }
    }

    Ok(ret)
}

#[cfg(any(target_os="linux", target_os="freebsd"))]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d\d?(m|d|u)?.so").unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os="macos")]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d\d?(m|d|u)?.(dylib|so)$").unwrap();
    }
    RE.is_match(pathname) || is_python_framework(pathname)
}

#[cfg(windows)]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = RegexBuilder::new(r"\\python\d\d\d?(m|d|u)?.dll$").case_insensitive(true).build().unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os="macos")]
pub fn is_python_framework(pathname: &str) -> bool {
    pathname.ends_with("/Python")  &&
    !pathname.contains("Python.app")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os="macos")]
    #[test]
    fn test_is_python_lib() {
        assert!(is_python_lib("~/Anaconda2/lib/libpython2.7.dylib"));

        // python lib configured with --with-pydebug (flag: d)
        assert!(is_python_lib("/lib/libpython3.4d.dylib"));

        // configured --with-pymalloc (flag: m)
        assert!(is_python_lib("/usr/local/lib/libpython3.8m.dylib"));

        // python2 configured with --with-wide-unicode (flag: u)
        assert!(is_python_lib("./libpython2.7u.dylib"));

        assert!(!is_python_lib("/libboost_python.dylib"));
        assert!(!is_python_lib("/lib/heapq.cpython-36m-darwin.dylib"));
    }

    #[cfg(any(target_os="linux", target_os="freebsd"))]
    #[test]
    fn test_is_python_lib() {
        // libpython bundled by pyinstaller https://github.com/benfred/py-spy/issues/42
        assert!(is_python_lib("/tmp/_MEIOqzg01/libpython2.7.so.1.0"));

        // test debug/malloc/unicode flags
        assert!(is_python_lib("./libpython2.7.so"));
        assert!(is_python_lib("/usr/lib/libpython3.4d.so"));
        assert!(is_python_lib("/usr/local/lib/libpython3.8m.so"));
        assert!(is_python_lib("/usr/lib/libpython2.7u.so"));

        // don't blindly match libraries with python in the name (boost_python etc)
        assert!(!is_python_lib("/usr/lib/libboost_python.so"));
        assert!(!is_python_lib("/usr/lib/x86_64-linux-gnu/libboost_python-py27.so.1.58.0"));
        assert!(!is_python_lib("/usr/lib/libboost_python-py35.so"));

    }

    #[cfg(windows)]
    #[test]
    fn test_is_python_lib() {
        assert!(is_python_lib("C:\\Users\\test\\AppData\\Local\\Programs\\Python\\Python37\\python37.dll"));
        // .NET host via https://github.com/pythonnet/pythonnet
        assert!(is_python_lib("C:\\Users\\test\\AppData\\Local\\Programs\\Python\\Python37\\python37.DLL"));
    }


    #[cfg(target_os="macos")]
    #[test]
    fn test_python_frameworks() {
        // homebrew v2
        assert!(!is_python_framework("/usr/local/Cellar/python@2/2.7.15_1/Frameworks/Python.framework/Versions/2.7/Resources/Python.app/Contents/MacOS/Python"));
        assert!(is_python_framework("/usr/local/Cellar/python@2/2.7.15_1/Frameworks/Python.framework/Versions/2.7/Python"));

        // System python from osx 10.13.6 (high sierra)
        assert!(!is_python_framework("/System/Library/Frameworks/Python.framework/Versions/2.7/Resources/Python.app/Contents/MacOS/Python"));
        assert!(is_python_framework("/System/Library/Frameworks/Python.framework/Versions/2.7/Python"));

        // pyenv 3.6.6 with OSX framework enabled (https://github.com/benfred/py-spy/issues/15)
        // env PYTHON_CONFIGURE_OPTS="--enable-framework" pyenv install 3.6.6
        assert!(is_python_framework("/Users/ben/.pyenv/versions/3.6.6/Python.framework/Versions/3.6/Python"));
        assert!(!is_python_framework("/Users/ben/.pyenv/versions/3.6.6/Python.framework/Versions/3.6/Resources/Python.app/Contents/MacOS/Python"));

        // single file pyinstaller
        assert!(is_python_framework("/private/var/folders/3x/qy479lpd1fb2q88lc9g4d3kr0000gn/T/_MEI2Akvi8/Python"));
    }
}
