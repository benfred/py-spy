use std;
use std::collections::HashMap;
use std::mem::size_of;
use std::slice;
use std::path::Path;
use regex::Regex;

use failure::{Error, ResultExt};
use remoteprocess::{Process, ProcessMemory, Pid};
use proc_maps::{get_process_maps, MapRange};
use python_bindings::{pyruntime, v2_7_15, v3_3_7, v3_5_5, v3_6_6, v3_7_0};

use binary_parser::{parse_binary, BinaryInfo};
use config::Config;
use native_stack_trace::NativeStack;
use python_interpreters::{self, InterpreterState, ThreadState};
use stack_trace::{StackTrace, get_stack_traces};
use version::Version;


pub struct PythonSpy {
    pub pid: Pid,
    pub process: Process,
    pub version: Version,
    pub interpreter_address: usize,
    pub threadstate_address: usize,
    pub python_filename: String,
    pub version_string: String,
    pub config: Config,
    pub native: Option<NativeStack>,
    pub short_filenames: HashMap<String, Option<String>>,
}

impl PythonSpy {
    pub fn new(pid: Pid, config: &Config) -> Result<PythonSpy, Error> {
        let process = remoteprocess::Process::new(pid)
            .context("Failed to open process - check if it is running.")?;

        // get basic process information (memory maps/symbols etc)
        let python_info = PythonProcessInfo::new(&process)?;

        let version = get_python_version(&python_info, &process)?;
        info!("python version {} detected", version);

        let interpreter_address = get_interpreter_address(&python_info, &process, &version)?;
        info!("Found interpreter at 0x{:016x}", interpreter_address);

        // lets us figure out which thread has the GIL
         let threadstate_address = match version {
             Version{major: 3, minor: 7...8, ..} => {
                match python_info.get_symbol("_PyRuntime") {
                    Some(&addr) => {
                        if let Some(offset) = pyruntime::get_tstate_current_offset(&version) {
                            info!("Found _PyRuntime @ 0x{:016x}, getting gilstate.tstate_current from offset 0x{:x}",
                                addr, offset);
                            addr as usize + offset
                        } else {
                            warn!("Unknown pyruntime.gilstate.tstate_current offset for version {:?}", version);
                            0
                        }
                    },
                    None => {
                        warn!("Failed to find _PyRuntime symbol - won't be able to detect GIL usage");
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
                        warn!("Failed to find _PyThreadState_Current symbol - won't be able to detect GIL usage");
                        0
                    }
                }
             }
         };

        let version_string = format!("python{}.{}", version.major, version.minor);

        let native = if config.native {
            Some(NativeStack::new(pid, &python_info.python_filename, &python_info.libpython_filename)?)
        } else {
            None
        };

        Ok(PythonSpy{pid, process, version, interpreter_address, threadstate_address,
                     python_filename: python_info.python_filename,
                     version_string,
                     native,
                     config: config.clone(),
                     short_filenames: HashMap::new()})
    }

    /// Creates a PythonSpy object, retrying up to max_retries times
    /// mainly useful for the case where the process is just started and
    /// symbols/python interpreter might not be loaded yet
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
        // lock the process if appropiate. note that native stack traces
        // mean that we lock separately. todo: fix this up
        // (gil check should be inside lock for native etc)
        let _lock = if self.config.non_blocking || self.config.native {
            None
        } else {
            Some(self.process.lock().context("Failed to suspend process")?)
        };

        match self.version {
            // Currently 3.7.x and 3.8.0a0 have the same ABI, but this might change
            // as 3.8 evolves
            Version{major: 3, minor: 8, ..} => self._get_stack_traces::<v3_7_0::_is>(),
            Version{major: 3, minor: 7, ..} => self._get_stack_traces::<v3_7_0::_is>(),
            Version{major: 3, minor: 6, ..} => self._get_stack_traces::<v3_6_6::_is>(),
            // ABI for 3.4 and 3.5 is the same for our purposes
            Version{major: 3, minor: 5, ..} => self._get_stack_traces::<v3_5_5::_is>(),
            Version{major: 3, minor: 4, ..} => self._get_stack_traces::<v3_5_5::_is>(),
            Version{major: 3, minor: 3, ..} => self._get_stack_traces::<v3_3_7::_is>(),
            // ABI for 2.3/2.4/2.5/2.6/2.7 is also compatible
            Version{major: 2, minor: 3...7, ..} => self._get_stack_traces::<v2_7_15::_is>(),
            _ => Err(format_err!("Unsupported version of Python: {}", self.version)),
        }
    }

    // implementation of get_stack_traces, where we have a type for the InterpreterState
    fn _get_stack_traces<I: InterpreterState>(&mut self) -> Result<Vec<StackTrace>, Error> {
        // figure out what thread has the GIL by inspecting _PyThreadState_Current
        let mut gil_thread_id = 0;
        if self.threadstate_address > 0 {
            let addr: usize = self.process.copy_struct(self.threadstate_address)?;
            if addr != 0 {
                match self.process.copy_struct::<I::ThreadState>(addr) {
                    Ok(threadstate) => { gil_thread_id = threadstate.thread_id(); },
                    Err(e) => { warn!("failed to copy threadstate: addr {:016x}. Err {:?}", addr, e); }
                }
            }
        }

        // Get the stack traces for each thread
        let interp: I = self.process.copy_struct(self.interpreter_address)
            .context("Failed to copy PyInterpreterState from process")?;

        let mut traces = match self.native.as_mut() {
            Some(native) => native.get_native_stack_traces(&interp, &self.process)?,
            None => get_stack_traces(&interp, &self.process)?
        };

        // annotate traces to indicate which thread is holding the gil (if any),
        // and to provide a shortened filename
        for trace in &mut traces {
            if trace.thread_id == gil_thread_id {
                trace.owns_gil = true;
            }
            for frame in &mut trace.frames {
                frame.short_filename = self.shorten_filename(&frame.filename);
            }
        }
        Ok(traces)
    }

    /// We want to display filenames without the boilerplate of the python installation
    /// directory etc. This function looks only includes paths inside a python
    /// package or subpackage, and not the path the package is installed at
    pub fn shorten_filename(&mut self, filename: &str) -> Option<String> {
        // if we have figured out the short filename already, use it
        if let Some(short) = self.short_filenames.get(filename) {
            return short.clone();
        }

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
        return Ok(Version::scan_bytes(&process.copy(addr as usize, 128)?)?);
    }

    // otherwise get version info from scanning BSS section for sys.version string
    info!("Getting version from python binary BSS");
    let bss = process.copy(python_info.python_binary.bss_addr as usize,
                           python_info.python_binary.bss_size as usize)?;
    match Version::scan_bytes(&bss) {
        Ok(version) => return Ok(version),
        Err(err) => {
            info!("Failed to get version from BSS section: {}", err);
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
        }
    }

    // the python_filename might have the version encoded in it (/usr/bin/python3.5 etc).
    // try reading that in (will miss patch level on python, but that shouldn't matter)
    info!("Trying to get version from path: {}", python_info.python_filename);
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
        Version{major: 3, minor: 7, ..} => {
            if let Some(&addr) = python_info.get_symbol("_PyRuntime") {
                let addr = process.copy_struct(addr as usize + pyruntime::INTERP_HEAD_OFFSET)?;

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
    match get_interpreter_address_from_binary(&python_info.python_binary, &python_info.maps, process, version) {
        Ok(addr) => Ok(addr),
        // Before giving up, try again if there is a libpython.so
        Err(err) => {
            match python_info.libpython_binary {
                Some(ref libpython) => {
                    info!("Failed to get interpreter from binary BSS, scanning libpython BSS");
                    Ok(get_interpreter_address_from_binary(libpython, &python_info.maps, process, version)?)
                },
                None => Err(err)
            }
        }
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
                    if thread.interp() as usize == addr && get_stack_traces(&interp, process).is_ok() {
                        return Ok(addr);
                    }
                }
            }
        }
        Err(format_err!("Failed to find a python interpreter in the .data section"))
    }

    // different versions have different layouts, check as appropiate
    match version {
        Version{major: 3, minor: 8, ..} => check::<v3_7_0::_is>(addrs, maps, process),
        Version{major: 3, minor: 7, ..} => check::<v3_7_0::_is>(addrs, maps, process),
        Version{major: 3, minor: 6, ..} => check::<v3_6_6::_is>(addrs, maps, process),
        Version{major: 3, minor: 5, ..} => check::<v3_5_5::_is>(addrs, maps, process),
        Version{major: 3, minor: 4, ..} => check::<v3_5_5::_is>(addrs, maps, process),
        Version{major: 3, minor: 3, ..} => check::<v3_3_7::_is>(addrs, maps, process),
        Version{major: 2, minor: 3...7, ..} => check::<v2_7_15::_is>(addrs, maps, process),
        _ => Err(format_err!("Unsupported version of Python: {}", version))
    }
}

/// Holds information about the python process: memory map layout, parsed binary info
/// for python /libpython etc.
pub struct PythonProcessInfo {
    python_binary: BinaryInfo,
    // if python was compiled with './configure --enabled-shared', code/symbols will
    // be in a libpython.so file instead of the executable. support that.
    libpython_binary: Option<BinaryInfo>,
    maps: Vec<MapRange>,
    python_filename: String,
    #[allow(dead_code)]
    libpython_filename: Option<String>,
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
            info!("map: {:016x}-{:016x} {}{}{} {}", map.start(), map.start() + map.size(),
                if map.is_read() {'r'} else {'-'}, if map.is_write() {'w'} else {'-'}, if map.is_exec() {'x'} else {'-'},
                map.filename().as_ref().unwrap_or(&"".to_owned()));
        }

        // on linux, support profiling processes running in docker containers by setting
        // the namespace to match that of the target process when reading in binaries
        #[cfg(target_os="linux")]
        let _namespace = match remoteprocess::Namespace::new(process.pid) {
            Ok(ns) => Some(ns),
            Err(e) => {
                warn!("Failed to set namespace: {}", e);
                None
            }
        };

        // parse the main python binary
        let (python_binary, python_filename) = {
            // Get the memory address for the executable by matching against virtual memory maps
            let map = maps.iter()
                .find(|m| if let Some(pathname) = &m.filename() {
                    is_python_bin(pathname) && m.is_exec()
                } else {
                    false
                });

            let map = match map {
                Some(map) => map,
                None => {
                    warn!("Failed to find '{}' in virtual memory maps, falling back to first map region", filename);
                    // If we failed to find the executable in the virtual memory maps, just take the first file we find
                    // sometimes on windows get_process_exe returns stale info =( https://github.com/benfred/py-spy/issues/40
                    // and on all operating systems I've tried, the exe is the first region in the maps
                    &maps[0]
                }
            };

            // TODO: consistent types? u64 -> usize? for map.start etc
            let mut python_binary = parse_binary(&filename, map.start() as u64)?;

            // windows symbols are stored in separate files (.pdb), load
            #[cfg(windows)]
            python_binary.symbols.extend(get_windows_python_symbols(process.pid, &filename, map.start() as u64)?);

            // For OSX, need to adjust main binary symbols by substracting _mh_execute_header
            // (which we've added to by map.start already, so undo that here)
            #[cfg(target_os = "macos")]
            {
                let offset = python_binary.symbols["_mh_execute_header"] - map.start() as u64;
                for address in python_binary.symbols.values_mut() {
                    *address -= offset;
                }

                if python_binary.bss_addr != 0 {
                    python_binary.bss_addr -= offset;
                }
            }
            (python_binary, filename.clone())
        };

        // likewise handle libpython for python versions compiled with --enabled-shared
        let mut libpython_filename = None;
        let libpython_binary = {
            let libmap = maps.iter()
                .find(|m| if let Some(ref pathname) = &m.filename() {
                    is_python_lib(pathname) && m.is_exec()
                } else {
                    false
                });

            let mut libpython_binary: Option<BinaryInfo> = None;
            if let Some(libpython) = libmap {
                if let Some(filename) = &libpython.filename() {
                    info!("Found libpython binary @ {}", filename);
                    let mut parsed = parse_binary(filename, libpython.start() as u64)?;
                    #[cfg(windows)]
                    parsed.symbols.extend(get_windows_python_symbols(process.pid, filename, libpython.start() as u64)?);
                    libpython_binary = Some(parsed);
                    libpython_filename = libpython.filename().clone();
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
                        info!("dyld: {:016x}-{:016x} {:10} {}",
                            dyld.segment.vmaddr, dyld.segment.vmaddr + dyld.segment.vmsize,
                            segname.to_string_lossy(), dyld.filename);
                    }

                    let python_dyld_data = dyld_infos.iter()
                        .find(|m| is_python_framework(&m.filename) &&
                                  m.segment.segname[0..7] == [95, 95, 68, 65, 84, 65, 0]);

                    if let Some(libpython) = python_dyld_data {
                        info!("Found libpython binary from dyld @ {}", libpython.filename);

                        let mut binary = parse_binary(&libpython.filename, libpython.segment.vmaddr)?;

                        // TODO: bss addr offsets returned from parsing binary are wrong
                        // (assumes data section isn't split from text section like done here).
                        // BSS occurs somewhere in the data section, just scan that
                        // (could later tighten this up to look at segment sections too)
                        binary.bss_addr = libpython.segment.vmaddr;
                        binary.bss_size = libpython.segment.vmsize;
                        libpython_binary = Some(binary);
                        libpython_filename = Some(libpython.filename.clone());
                    }
                }
            }

            libpython_binary
        };

        Ok(PythonProcessInfo{python_binary, libpython_binary, maps, python_filename, libpython_filename})
    }

    pub fn get_symbol(&self, symbol: &str) -> Option<&u64> {
        if let Some(addr) = self.python_binary.symbols.get(symbol) {
            info!("got symbol {} (0x{:016x}) from python binary", symbol, addr);
            return Some(addr);
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

// We can't use goblin to parse external symbol files (like in a separate .pdb file) on windows,
// So use the win32 api to load up the couple of symbols we need on windows. Note:
// we still can get export's from the PE file
#[cfg(windows)]
pub fn get_windows_python_symbols(pid: Pid, filename: &str, offset: u64) -> std::io::Result<HashMap<String, u64>> {
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

#[cfg(target_os="linux")]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d(m|d|u)?.so").unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os="macos")]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d(m|d|u)?.(dylib|so)$").unwrap();
    }
    RE.is_match(pathname) || is_python_framework(pathname)
}

#[cfg(windows)]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"\\python\d\d(m|d|u)?.dll$").unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os="macos")]
pub fn is_python_framework(pathname: &str) -> bool {
    pathname.ends_with("/Python") &&
    pathname.contains("/Python.framework/") &&
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

    #[cfg(target_os="linux")]
    #[test]
    fn test_is_python_lib() {
        // libpython bundled by pyinstaller https://github.com/benfred/py-spy/issues/42
        assert!(is_python_lib("/tmp/_MEIOqzg01/libpython2.7.so.1.0"));

        // test debug/malloc/unicode flags
        assert!(is_python_lib("./libpython2.7.so"));
        assert!(is_python_lib("/usr/lib/libpython3.4d.so"));
        assert!(is_python_lib("/usr/local/lib/libpython3.8m.so"));
        assert!(is_python_lib("/usr/lib/libpython2.7u.so"));

        // don't blindly match libraries with pytohn in the name (boost_python etc)
        assert!(!is_python_lib("/usr/lib/libboost_python.so"));
        assert!(!is_python_lib("/usr/lib/x86_64-linux-gnu/libboost_python-py27.so.1.58.0"));
        assert!(!is_python_lib("/usr/lib/libboost_python-py35.so"));

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
    }
}
