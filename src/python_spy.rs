use std;
use std::mem::size_of;
use std::slice;
use std::path::Path;
use regex::Regex;

use failure::{Error, ResultExt};
use read_process_memory::{Pid, TryIntoProcessHandle, copy_address, ProcessHandle};
use proc_maps::{get_process_maps, MapRange};
use python_bindings::{v2_7_15, v3_3_7, v3_5_5, v3_6_6, v3_7_0};

use python_interpreters;
use stack_trace::{StackTrace, get_stack_traces};
use binary_parser::{parse_binary, BinaryInfo};
use utils::{copy_struct, copy_pointer, get_process_exe};
use python_interpreters::{InterpreterState, ThreadState};

#[derive(Debug)]
pub struct PythonSpy {
    pub pid: Pid,
    pub process: ProcessHandle,
    pub version: Version,
    pub interpreter_address: usize,
    pub threadstate_address: usize,
    pub python_filename: String,
    pub python_install_path: String,
    pub version_string: String
}

impl PythonSpy {
    pub fn new(pid: Pid) -> Result<PythonSpy, Error> {
        // get basic process information (memory maps/symbols etc)
        let python_info = PythonProcessInfo::new(pid)?;

        let process = pid.try_into_process_handle().context("Failed to open target process")?;
        let version = get_python_version(&python_info, process)?;
        info!("python version {} detected", version);

        let interpreter_address = get_interpreter_address(&python_info, process, &version)?;
        info!("Found interpreter at 0x{:016x}", interpreter_address);

        // lets us figure out which thread has the GIL
        let threadstate_address = match python_info.get_symbol("_PyThreadState_Current") {
            Some(&addr) => {
                info!("Found _PyThreadState_Current @ 0x{:016x}", addr);
                addr as usize
            },
            None => {
                warn!("Failed to find _PyThreadState_Current symbol - won't be able to detect GIL usage");
                0
            }
        };

        // Figure out the base path of the python install
        let python_install_path = {
            let mut python_path = Path::new(&python_info.python_filename);
            if let Some(parent) = python_path.parent() {
                python_path = parent;
                if python_path.to_str().unwrap().ends_with("/bin") {
                    if let Some(parent) = python_path.parent() {
                        python_path = parent;
                    }
                }
            }
            python_path.to_str().unwrap().to_string()
        };

        let version_string = format!("python{}.{}", version.major, version.minor);

        Ok(PythonSpy{pid, process, version, interpreter_address, threadstate_address,
                     python_filename: python_info.python_filename,
                     python_install_path,
                     version_string})
    }

    /// Creates a PythonSpy object, retrying up to max_retries times
    /// mainly useful for the case where the process is just started and
    /// symbols/python interpreter might not be loaded yet
    pub fn retry_new(pid: Pid, max_retries:u64) -> Result<PythonSpy, Error> {
        let mut retries = 0;
        loop {
            let err = match PythonSpy::new(pid) {
                Ok(process) => {
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
    pub fn get_stack_traces(&self) -> Result<Vec<StackTrace>, Error> {
        match self.version {
            // Currently 3.7.x and 3.8.0a0 have the same ABI, but this might change
            // as 3.8 evolvess
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
    fn _get_stack_traces<I: InterpreterState>(&self) -> Result<Vec<StackTrace>, Error> {
        // figure out what thread has the GIL by inspecting _PyThreadState_Current
        let mut gil_thread_id = 0;
        if self.threadstate_address > 0 {
            let addr: usize = copy_struct(self.threadstate_address, &self.process)?;
            if addr != 0 {
                let threadstate: I::ThreadState = copy_struct(addr, &self.process)?;
                gil_thread_id = threadstate.thread_id();
            }
        }

        // Get the stack traces for each thread
        let interp: I = copy_struct(self.interpreter_address, &self.process)
            .context("Failed to copy PyInterpreterState from process")?;
        let mut traces = get_stack_traces(&interp, &self.process)?;

        // annotate traces to indicate which thread is holding the gil (if any),
        // and to provide a shortened filename
        for trace in &mut traces {
            if trace.thread_id == gil_thread_id {
                trace.owns_gil = true;
            }
            for frame in &mut trace.frames {
                frame.short_filename = Some(self.shorten_filename(&frame.filename).to_owned());
            }
        }
        Ok(traces)
    }

    /// We want to display filenames without the boilerplate of the python installation
    /// directory etc. This strips off common prefixes from python library code.
    pub fn shorten_filename<'a>(&self, filename: &'a str) -> &'a str {
        if filename.starts_with(&self.python_install_path) {
            let mut filename = &filename[self.python_install_path.len() + 1..];
            if filename.starts_with("lib") {
                filename = &filename[4..];
                if filename.starts_with(&self.version_string) {
                    filename = &filename[self.version_string.len() + 1..];
                }
                if filename.starts_with("site-packages") {
                    filename = &filename[14..];
                }
            }
            filename
        } else {
            filename
        }
    }
}
/// Returns the version of python running in the process.
fn get_python_version(python_info: &PythonProcessInfo, process: ProcessHandle)
        -> Result<Version, Error> {
    // If possible, grab the sys.version string from the processes memory (mac osx).
    if let Some(&addr) = python_info.get_symbol("Py_GetVersion.version") {
        info!("Getting version from symbol address");
        return Ok(Version::scan_bytes(&copy_address(addr as usize, 128, &process)?)?);
    }

    // otherwise get version info from scanning BSS section for sys.version string
    info!("Getting version from python binary BSS");
    let bss = copy_address(python_info.python_binary.bss_addr as usize,
                           python_info.python_binary.bss_size as usize, &process)?;
    match Version::scan_bytes(&bss) {
        Ok(version) => return Ok(version),
        Err(err) => {
            info!("Failed to get version from BSS section: {}", err);
            // try again if there is a libpython.so
            if let Some(ref libpython) = python_info.libpython_binary {
                info!("Getting version from libpython BSS");
                let bss = copy_address(libpython.bss_addr as usize,
                                        libpython.bss_size as usize, &process)?;
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
    let path = std::path::Path::new(&python_info.python_filename);
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
                           process: ProcessHandle,
                           version: &Version) -> Result<usize, Error> {
    // get the address of the main PyInterpreterState object from loaded symbols if we can
    // (this tends to be faster than scanning through the bss section)
    match version {
        Version{major: 3, minor: 7, ..} => {
            if let Some(&addr) = python_info.get_symbol("_PyRuntime") {
                let addr = copy_struct((addr + 24) as usize, &process)?;
                // Make sure the interpreter addr is valid before returning
                match check_interpreter_addresses(&[addr], &python_info.maps, process, version) {
                    Ok(addr) => return Ok(addr),
                    Err(_) => { warn!("Interpreter address from _PyRuntime symbol is invalid {:016x}", addr); }
                };
            }
        },
        _ => {
            if let Some(&addr) = python_info.get_symbol("interp_head") {
                let addr = copy_struct(addr as usize, &process)?;
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
                                       process: ProcessHandle,
                                       version: &Version) -> Result<usize, Error> {
    // We're going to scan the BSS/data section for things, and try to narrowly scan things that
    // look like pointers to PyinterpreterState
    let bss = copy_address(binary.bss_addr as usize, binary.bss_size as usize, &process)?;

    #[cfg_attr(feature = "cargo-clippy", allow(cast_ptr_alignment))]
    let addrs = unsafe { slice::from_raw_parts(bss.as_ptr() as *const usize, bss.len() / size_of::<usize>()) };
    check_interpreter_addresses(addrs, maps, process, version)
}

// Checks whether a block of memory (from BSS/.data etc) contains pointers that are pointing
// to a valid PyInterpreterState
fn check_interpreter_addresses(addrs: &[usize],
                               maps: &[MapRange],
                               process: ProcessHandle,
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
                process: ProcessHandle) -> Result<usize, Error>
            where I: python_interpreters::InterpreterState {
        for &addr in addrs {
            if maps_contain_addr(addr, maps) {
                // this address points to valid memory. try loading it up as a PyInterpreterState
                // to further check
                let interp: I = match copy_struct(addr, &process) {
                    Ok(interp) => interp,
                    Err(_) => continue
                };

                // get the pythreadstate pointer from the interpreter object, and if it is also
                // a valid pointer then load it up.
                let threads = interp.head();
                if maps_contain_addr(threads as usize, maps) {
                    // If the threadstate points back to the interpreter like we expect, then
                    // this is almost certainly the address of the intrepreter
                    let thread = match copy_pointer(threads, &process) {
                        Ok(thread) => thread,
                        Err(_) => continue
                    };

                    // as a final sanity check, try getting the stack_traces, and only return if this works
                    if thread.interp() as usize == addr && get_stack_traces(&interp, &process).is_ok() {
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
}

impl PythonProcessInfo {
    fn new(pid: Pid) -> Result<PythonProcessInfo, Error> {
        // Get the executable filename for the process
        let filename = get_process_exe(pid)
            .context("Failed to get process executable name. Check that the process is running.")?;
        info!("Found process binary @ '{}'", filename);

        #[cfg(windows)]
        let filename = filename.to_lowercase();
        #[cfg(windows)]
        let is_python_bin = |pathname: &str| pathname.to_lowercase() == filename;

        #[cfg(not(windows))]
        let is_python_bin = |pathname: &str| pathname == filename;

        // get virtual memory layout
        let maps = get_process_maps(pid)?;
        info!("Got virtual memory maps from pid {}:", pid);
        for map in &maps {
            info!("map: {:016x}-{:016x} {}{}{} {}", map.start(), map.start() + map.size(),
                if map.is_read() {'r'} else {'-'}, if map.is_write() {'w'} else {'-'}, if map.is_exec() {'x'} else {'-'},
                map.filename().as_ref().unwrap_or(&"".to_owned()));
        }

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
            python_binary.symbols.extend(get_windows_python_symbols(pid, &filename, map.start() as u64)?);

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
                    parsed.symbols.extend(get_windows_python_symbols(pid, filename, libpython.start() as u64)?);
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
                    let dyld_infos = get_dyld_info(pid)?;

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
                    }
                }
            }

            libpython_binary
        };

        Ok(PythonProcessInfo{python_binary, libpython_binary, maps, python_filename})
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
use std::collections::HashMap;
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

#[derive(Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub release_flags: String
}

impl Version {
    pub fn scan_bytes(data: &[u8]) -> Result<Version, Error> {
        use regex::bytes::Regex;
        lazy_static! {
            static ref RE: Regex = Regex::new(r"((2|3)\.(3|4|5|6|7|8)\.(\d{1,2}))((a|b|c|rc)\d{1,2})? (.{1,64})").unwrap();
        }

        if let Some(cap) = RE.captures_iter(data).next() {
            let release = match cap.get(5) {
                Some(x) => { std::str::from_utf8(x.as_bytes())? },
                None => ""
            };
            let major = std::str::from_utf8(&cap[2])?.parse::<u64>()?;
            let minor = std::str::from_utf8(&cap[3])?.parse::<u64>()?;
            let patch = std::str::from_utf8(&cap[4])?.parse::<u64>()?;

            let version = std::str::from_utf8(&cap[0])?;
            info!("Found matching version string '{}'", version);
            #[cfg(windows)]
            {
                if version.contains("32 bit") {
                    error!("32-bit python is not yet supported on windows! See https://github.com/benfred/py-spy/issues/31 for updates");
                    // we're panic'ing rather than returning an error, since we can't recover from this
                    // and returning an error would just get the calling code to fall back to other
                    // methods of trying to find the version
                    panic!("32-bit python is unsupported on windows");
                }
            }

            return Ok(Version{major, minor, patch, release_flags:release.to_owned()});
        }
        Err(format_err!("failed to find version string"))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}.{}{}", self.major, self.minor, self.patch, self.release_flags)
    }
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

    #[test]
    fn test_find_version() {
        let version = Version::scan_bytes(b"2.7.10 (default, Oct  6 2017, 22:29:07)").unwrap();
        assert_eq!(version, Version{major: 2, minor: 7, patch: 10, release_flags: "".to_owned()});

        let version = Version::scan_bytes(b"3.6.3 |Anaconda custom (64-bit)| (default, Oct  6 2017, 12:04:38)").unwrap();
        assert_eq!(version, Version{major: 3, minor: 6, patch: 3, release_flags: "".to_owned()});

        let version = Version::scan_bytes(b"Python 3.7.0rc1 (v3.7.0rc1:dfad352267, Jul 20 2018, 13:27:54)").unwrap();
        assert_eq!(version, Version{major: 3, minor: 7, patch: 0, release_flags: "rc1".to_owned()});

        let version = Version::scan_bytes(b"1.7.0rc1 (v1.7.0rc1:dfad352267, Jul 20 2018, 13:27:54)");
        assert!(version.is_err(), "don't match unsupported ");

        let version = Version::scan_bytes(b"3.7 10 ");
        assert!(version.is_err(), "needs dotted version");

        let version = Version::scan_bytes(b"3.7.10fooboo ");
        assert!(version.is_err(), "limit suffixes");
    }
}
