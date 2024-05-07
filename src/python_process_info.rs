use regex::Regex;
#[cfg(windows)]
use regex::RegexBuilder;
#[cfg(windows)]
use std::collections::HashMap;
use std::mem::size_of;
use std::path::Path;
use std::slice;

use anyhow::{Context, Error, Result};
use lazy_static::lazy_static;
use proc_maps::{get_process_maps, MapRange};
use remoteprocess::{Pid, ProcessMemory};

use crate::binary_parser::{parse_binary, BinaryInfo};
use crate::config::Config;
use crate::python_bindings::{
    pyruntime, v2_7_15, v3_10_0, v3_11_0, v3_3_7, v3_5_5, v3_6_6, v3_7_0, v3_8_0, v3_9_5,
};
use crate::python_interpreters::{InterpreterState, ThreadState};
use crate::stack_trace::get_stack_traces;
use crate::version::Version;

/// Holds information about the python process: memory map layout, parsed binary info
/// for python /libpython etc.
pub struct PythonProcessInfo {
    pub python_binary: Option<BinaryInfo>,
    // if python was compiled with './configure --enabled-shared', code/symbols will
    // be in a libpython.so file instead of the executable. support that.
    pub libpython_binary: Option<BinaryInfo>,
    pub maps: Box<dyn ContainsAddr>,
    pub python_filename: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    pub dockerized: bool,
}

impl PythonProcessInfo {
    pub fn new(process: &remoteprocess::Process) -> Result<PythonProcessInfo, Error> {
        let filename = process
            .exe()
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
            debug!(
                "map: {:016x}-{:016x} {}{}{} {}",
                map.start(),
                map.start() + map.size(),
                if map.is_read() { 'r' } else { '-' },
                if map.is_write() { 'w' } else { '-' },
                if map.is_exec() { 'x' } else { '-' },
                map.filename()
                    .unwrap_or(&std::path::PathBuf::from(""))
                    .display()
            );
        }

        // parse the main python binary
        let (python_binary, python_filename) = {
            // Get the memory address for the executable by matching against virtual memory maps
            let map = maps.iter().find(|m| {
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
                    maps.first().ok_or_else(|| {
                        format_err!("Failed to get virtual memory maps from process")
                    })?
                }
            };

            #[cfg(not(target_os = "linux"))]
            let filename = std::path::PathBuf::from(filename);

            // use filename through /proc/pid/exe which works across docker namespaces and
            // handles if the file was deleted
            #[cfg(target_os = "linux")]
            let filename = std::path::PathBuf::from(format!("/proc/{}/exe", process.pid));

            // TODO: consistent types? u64 -> usize? for map.start etc
            let python_binary = parse_binary(&filename, map.start() as u64, map.size() as u64);

            // windows symbols are stored in separate files (.pdb), load
            #[cfg(windows)]
            let python_binary = python_binary.and_then(|mut pb| {
                get_windows_python_symbols(process.pid, &filename, map.start() as u64)
                    .map(|symbols| {
                        pb.symbols.extend(symbols);
                        pb
                    })
                    .map_err(|err| err.into())
            });

            // For OSX, need to adjust main binary symbols by subtracting _mh_execute_header
            // (which we've added to by map.start already, so undo that here)
            #[cfg(target_os = "macos")]
            let python_binary = python_binary.map(|mut pb| {
                let offset = pb.symbols["_mh_execute_header"] - map.start() as u64;
                for address in pb.symbols.values_mut() {
                    *address -= offset;
                }

                if pb.bss_addr != 0 {
                    pb.bss_addr -= offset;
                }
                pb
            });

            (python_binary, filename)
        };

        // likewise handle libpython for python versions compiled with --enabled-shared
        let libpython_binary = {
            let libmap = maps.iter().find(|m| {
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

                    // on linux the process could be running in docker, access the filename through procfs
                    #[cfg(target_os = "linux")]
                    let filename = &std::path::PathBuf::from(format!(
                        "/proc/{}/root{}",
                        process.pid,
                        filename.display()
                    ));

                    #[allow(unused_mut)]
                    let mut parsed =
                        parse_binary(filename, libpython.start() as u64, libpython.size() as u64)?;
                    #[cfg(windows)]
                    parsed.symbols.extend(get_windows_python_symbols(
                        process.pid,
                        filename,
                        libpython.start() as u64,
                    )?);
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
                        let segname =
                            unsafe { std::ffi::CStr::from_ptr(dyld.segment.segname.as_ptr()) };
                        debug!(
                            "dyld: {:016x}-{:016x} {:10} {}",
                            dyld.segment.vmaddr,
                            dyld.segment.vmaddr + dyld.segment.vmsize,
                            segname.to_string_lossy(),
                            dyld.filename.display()
                        );
                    }

                    let python_dyld_data = dyld_infos.iter().find(|m| {
                        if let Some(filename) = m.filename.to_str() {
                            return is_python_framework(filename)
                                && m.segment.segname[0..7] == [95, 95, 68, 65, 84, 65, 0];
                        }
                        false
                    });

                    if let Some(libpython) = python_dyld_data {
                        info!(
                            "Found libpython binary from dyld @ {}",
                            libpython.filename.display()
                        );

                        let mut binary = parse_binary(
                            &libpython.filename,
                            libpython.segment.vmaddr,
                            libpython.segment.vmsize,
                        )?;

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

        #[cfg(target_os = "linux")]
        let dockerized = is_dockerized(process.pid).unwrap_or(false);

        Ok(PythonProcessInfo {
            python_binary,
            libpython_binary,
            maps: Box::new(maps),
            python_filename,
            #[cfg(target_os = "linux")]
            dockerized,
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
                info!(
                    "got symbol {} (0x{:016x}) from libpython binary",
                    symbol, addr
                );
                return Some(addr);
            }
        }
        None
    }
}

/// Returns the version of python running in the process.
pub fn get_python_version<P>(python_info: &PythonProcessInfo, process: &P) -> Result<Version, Error>
where
    P: ProcessMemory,
{
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
        let bss = process.copy(pb.bss_addr as usize, pb.bss_size as usize)?;
        match Version::scan_bytes(&bss) {
            Ok(version) => return Ok(version),
            Err(err) => info!("Failed to get version from BSS section: {}", err),
        }
    }

    // try again if there is a libpython.so
    if let Some(ref libpython) = python_info.libpython_binary {
        info!("Getting version from libpython BSS");
        let bss = process.copy(libpython.bss_addr as usize, libpython.bss_size as usize)?;
        match Version::scan_bytes(&bss) {
            Ok(version) => return Ok(version),
            Err(err) => info!("Failed to get version from libpython BSS section: {}", err),
        }
    }

    // the python_filename might have the version encoded in it (/usr/bin/python3.5 etc).
    // try reading that in (will miss patch level on python, but that shouldn't matter)
    info!(
        "Trying to get version from path: {}",
        python_info.python_filename.display()
    );
    let path = Path::new(&python_info.python_filename);
    if let Some(python) = path.file_name() {
        if let Some(python) = python.to_str() {
            if let Some(stripped_python) = python.strip_prefix("python") {
                let tokens: Vec<&str> = stripped_python.split('.').collect();
                if tokens.len() >= 2 {
                    if let (Ok(major), Ok(minor)) =
                        (tokens[0].parse::<u64>(), tokens[1].parse::<u64>())
                    {
                        return Ok(Version {
                            major,
                            minor,
                            patch: 0,
                            release_flags: "".to_owned(),
                            build_metadata: None,
                        });
                    }
                }
            }
        }
    }
    Err(format_err!(
        "Failed to find python version from target process"
    ))
}

pub fn get_interpreter_address<P>(
    python_info: &PythonProcessInfo,
    process: &P,
    version: &Version,
) -> Result<usize, Error>
where
    P: ProcessMemory,
{
    // get the address of the main PyInterpreterState object from loaded symbols if we can
    // (this tends to be faster than scanning through the bss section)
    match version {
        Version {
            major: 3,
            minor: 7..=11,
            ..
        } => {
            if let Some(&addr) = python_info.get_symbol("_PyRuntime") {
                let addr = process
                    .copy_struct(addr as usize + pyruntime::get_interp_head_offset(version))?;

                // Make sure the interpreter addr is valid before returning
                match check_interpreter_addresses(&[addr], &*python_info.maps, process, version) {
                    Ok(addr) => return Ok(addr),
                    Err(_) => {
                        warn!(
                            "Interpreter address from _PyRuntime symbol is invalid {:016x}",
                            addr
                        );
                    }
                };
            }
        }
        _ => {
            if let Some(&addr) = python_info.get_symbol("interp_head") {
                let addr = process.copy_struct(addr as usize)?;
                match check_interpreter_addresses(&[addr], &*python_info.maps, process, version) {
                    Ok(addr) => return Ok(addr),
                    Err(_) => {
                        warn!(
                            "Interpreter address from interp_head symbol is invalid {:016x}",
                            addr
                        );
                    }
                };
            }
        }
    };
    info!("Failed to get interp_head from symbols, scanning BSS section from main binary");

    // try scanning the BSS section of the binary for things that might be the interpreterstate
    let err = if let Some(ref pb) = python_info.python_binary {
        match get_interpreter_address_from_binary(pb, &*python_info.maps, process, version) {
            Ok(addr) => return Ok(addr),
            err => Some(err),
        }
    } else {
        None
    };
    // Before giving up, try again if there is a libpython.so
    if let Some(ref lpb) = python_info.libpython_binary {
        info!("Failed to get interpreter from binary BSS, scanning libpython BSS");
        match get_interpreter_address_from_binary(lpb, &*python_info.maps, process, version) {
            Ok(addr) => Ok(addr),
            lib_err => err.unwrap_or(lib_err),
        }
    } else {
        err.expect("Both python and libpython are invalid.")
    }
}

fn get_interpreter_address_from_binary<P>(
    binary: &BinaryInfo,
    maps: &dyn ContainsAddr,
    process: &P,
    version: &Version,
) -> Result<usize, Error>
where
    P: ProcessMemory,
{
    // We're going to scan the BSS/data section for things, and try to narrowly scan things that
    // look like pointers to PyinterpreterState
    let bss = process.copy(binary.bss_addr as usize, binary.bss_size as usize)?;

    #[allow(clippy::cast_ptr_alignment)]
    let addrs = unsafe {
        slice::from_raw_parts(bss.as_ptr() as *const usize, bss.len() / size_of::<usize>())
    };
    check_interpreter_addresses(addrs, maps, process, version)
}

// Checks whether a block of memory (from BSS/.data etc) contains pointers that are pointing
// to a valid PyInterpreterState
fn check_interpreter_addresses<P>(
    addrs: &[usize],
    maps: &dyn ContainsAddr,
    process: &P,
    version: &Version,
) -> Result<usize, Error>
where
    P: ProcessMemory,
{
    // This function does all the work, but needs a type of the interpreter
    fn check<I, P>(addrs: &[usize], maps: &dyn ContainsAddr, process: &P) -> Result<usize, Error>
    where
        I: InterpreterState,
        P: ProcessMemory,
    {
        for &addr in addrs {
            if maps.contains_addr(addr) {
                // this address points to valid memory. try loading it up as a PyInterpreterState
                // to further check
                let interp: I = match process.copy_struct(addr) {
                    Ok(interp) => interp,
                    Err(_) => continue,
                };

                // get the pythreadstate pointer from the interpreter object, and if it is also
                // a valid pointer then load it up.
                let threads = interp.head();
                if maps.contains_addr(threads as usize) {
                    // If the threadstate points back to the interpreter like we expect, then
                    // this is almost certainly the address of the intrepreter
                    let thread = match process.copy_pointer(threads) {
                        Ok(thread) => thread,
                        Err(_) => continue,
                    };

                    // as a final sanity check, try getting the stack_traces, and only return if this works
                    if thread.interp() as usize == addr
                        && get_stack_traces(&interp, process, 0, None).is_ok()
                    {
                        return Ok(addr);
                    }
                }
            }
        }
        Err(format_err!(
            "Failed to find a python interpreter in the .data section"
        ))
    }

    // different versions have different layouts, check as appropriate
    match version {
        Version {
            major: 2,
            minor: 3..=7,
            ..
        } => check::<v2_7_15::_is, P>(addrs, maps, process),
        Version {
            major: 3, minor: 3, ..
        } => check::<v3_3_7::_is, P>(addrs, maps, process),
        Version {
            major: 3,
            minor: 4..=5,
            ..
        } => check::<v3_5_5::_is, P>(addrs, maps, process),
        Version {
            major: 3, minor: 6, ..
        } => check::<v3_6_6::_is, P>(addrs, maps, process),
        Version {
            major: 3, minor: 7, ..
        } => check::<v3_7_0::_is, P>(addrs, maps, process),
        Version {
            major: 3,
            minor: 8,
            patch: 0,
            ..
        } => match version.release_flags.as_ref() {
            "a1" | "a2" | "a3" => check::<v3_7_0::_is, P>(addrs, maps, process),
            _ => check::<v3_8_0::_is, P>(addrs, maps, process),
        },
        Version {
            major: 3, minor: 8, ..
        } => check::<v3_8_0::_is, P>(addrs, maps, process),
        Version {
            major: 3, minor: 9, ..
        } => check::<v3_9_5::_is, P>(addrs, maps, process),
        Version {
            major: 3,
            minor: 10,
            ..
        } => check::<v3_10_0::_is, P>(addrs, maps, process),
        Version {
            major: 3,
            minor: 11,
            ..
        } => check::<v3_11_0::_is, P>(addrs, maps, process),
        _ => Err(format_err!("Unsupported version of Python: {}", version)),
    }
}

pub fn get_threadstate_address(
    python_info: &PythonProcessInfo,
    version: &Version,
    config: &Config,
) -> Result<usize, Error> {
    let threadstate_address = match version {
        Version {
            major: 3,
            minor: 7..=11,
            ..
        } => match python_info.get_symbol("_PyRuntime") {
            Some(&addr) => {
                if let Some(offset) = pyruntime::get_tstate_current_offset(version) {
                    info!("Found _PyRuntime @ 0x{:016x}, getting gilstate.tstate_current from offset 0x{:x}",
                            addr, offset);
                    addr as usize + offset
                } else {
                    error_if_gil(
                        config,
                        version,
                        "unknown pyruntime.gilstate.tstate_current offset",
                    )?;
                    0
                }
            }
            None => {
                error_if_gil(config, version, "failed to find _PyRuntime symbol")?;
                0
            }
        },
        _ => match python_info.get_symbol("_PyThreadState_Current") {
            Some(&addr) => {
                info!("Found _PyThreadState_Current @ 0x{:016x}", addr);
                addr as usize
            }
            None => {
                error_if_gil(
                    config,
                    version,
                    "failed to find _PyThreadState_Current symbol",
                )?;
                0
            }
        },
    };

    Ok(threadstate_address)
}

fn error_if_gil(config: &Config, version: &Version, msg: &str) -> Result<(), Error> {
    lazy_static! {
        static ref WARNED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
    }

    if config.gil_only {
        if !WARNED.load(std::sync::atomic::Ordering::Relaxed) {
            // only print this once
            eprintln!(
                "Cannot detect GIL holding in version '{}' on the current platform (reason: {})",
                version, msg
            );
            eprintln!("Please open an issue in https://github.com/benfred/py-spy with the Python version and your platform.");
            WARNED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Err(format_err!(
            "Cannot detect GIL holding in version '{}' on the current platform (reason: {})",
            version,
            msg
        ))
    } else {
        warn!("Unable to detect GIL usage: {}", msg);
        Ok(())
    }
}

pub trait ContainsAddr {
    fn contains_addr(&self, addr: usize) -> bool;
}

impl ContainsAddr for Vec<MapRange> {
    #[cfg(windows)]
    fn contains_addr(&self, addr: usize) -> bool {
        // On windows, we can't just check if a pointer is valid by looking to see if it points
        // to something in the virtual memory map. Brute-force it instead
        true
    }

    #[cfg(not(windows))]
    fn contains_addr(&self, addr: usize) -> bool {
        proc_maps::maps_contain_addr(addr, self)
    }
}

#[cfg(target_os = "linux")]
fn is_dockerized(pid: Pid) -> Result<bool, Error> {
    let self_mnt = std::fs::read_link("/proc/self/ns/mnt")?;
    let target_mnt = std::fs::read_link(format!("/proc/{}/ns/mnt", pid))?;
    Ok(self_mnt != target_mnt)
}

// We can't use goblin to parse external symbol files (like in a separate .pdb file) on windows,
// So use the win32 api to load up the couple of symbols we need on windows. Note:
// we still can get export's from the PE file
#[cfg(windows)]
pub fn get_windows_python_symbols(
    pid: Pid,
    filename: &Path,
    offset: u64,
) -> std::io::Result<HashMap<String, u64>> {
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
            let addr = if base == 0 {
                addr
            } else {
                offset + addr - base
            };
            ret.insert(String::from(*symbol), addr);
        }
    }

    Ok(ret)
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d\d?(m|d|u)?.so").unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os = "macos")]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = Regex::new(r"/libpython\d.\d\d?(m|d|u)?.(dylib|so)$").unwrap();
    }
    RE.is_match(pathname) || is_python_framework(pathname)
}

#[cfg(windows)]
pub fn is_python_lib(pathname: &str) -> bool {
    lazy_static! {
        static ref RE: Regex = RegexBuilder::new(r"\\python\d\d\d?(m|d|u)?.dll$")
            .case_insensitive(true)
            .build()
            .unwrap();
    }
    RE.is_match(pathname)
}

#[cfg(target_os = "macos")]
pub fn is_python_framework(pathname: &str) -> bool {
    pathname.ends_with("/Python") && !pathname.contains("Python.app")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
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

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
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
        assert!(!is_python_lib(
            "/usr/lib/x86_64-linux-gnu/libboost_python-py27.so.1.58.0"
        ));
        assert!(!is_python_lib("/usr/lib/libboost_python-py35.so"));
    }

    #[cfg(windows)]
    #[test]
    fn test_is_python_lib() {
        assert!(is_python_lib(
            "C:\\Users\\test\\AppData\\Local\\Programs\\Python\\Python37\\python37.dll"
        ));
        // .NET host via https://github.com/pythonnet/pythonnet
        assert!(is_python_lib(
            "C:\\Users\\test\\AppData\\Local\\Programs\\Python\\Python37\\python37.DLL"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_python_frameworks() {
        // homebrew v2
        assert!(!is_python_framework("/usr/local/Cellar/python@2/2.7.15_1/Frameworks/Python.framework/Versions/2.7/Resources/Python.app/Contents/MacOS/Python"));
        assert!(is_python_framework(
            "/usr/local/Cellar/python@2/2.7.15_1/Frameworks/Python.framework/Versions/2.7/Python"
        ));

        // System python from osx 10.13.6 (high sierra)
        assert!(!is_python_framework("/System/Library/Frameworks/Python.framework/Versions/2.7/Resources/Python.app/Contents/MacOS/Python"));
        assert!(is_python_framework(
            "/System/Library/Frameworks/Python.framework/Versions/2.7/Python"
        ));

        // pyenv 3.6.6 with OSX framework enabled (https://github.com/benfred/py-spy/issues/15)
        // env PYTHON_CONFIGURE_OPTS="--enable-framework" pyenv install 3.6.6
        assert!(is_python_framework(
            "/Users/ben/.pyenv/versions/3.6.6/Python.framework/Versions/3.6/Python"
        ));
        assert!(!is_python_framework("/Users/ben/.pyenv/versions/3.6.6/Python.framework/Versions/3.6/Resources/Python.app/Contents/MacOS/Python"));

        // single file pyinstaller
        assert!(is_python_framework(
            "/private/var/folders/3x/qy479lpd1fb2q88lc9g4d3kr0000gn/T/_MEI2Akvi8/Python"
        ));
    }
}
