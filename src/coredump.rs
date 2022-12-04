use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::io::Read;

use anyhow::{Error, Context, Result};
use console::{Term, style};
use goblin;
use log::{info};
use libc;
use remoteprocess;
use remoteprocess::ProcessMemory;

use crate::binary_parser::{BinaryInfo, parse_binary};
use crate::python_bindings::{pyruntime, v2_7_15, v3_3_7, v3_5_5, v3_6_6, v3_7_0, v3_8_0, v3_9_5, v3_10_0, v3_11_0};
use crate::python_interpreters::InterpreterState;
use crate::stack_trace::{StackTrace, get_stack_traces, get_stack_trace};
use crate::python_threading::thread_names_from_interpreter;
use crate::version::Version;
use crate::config::{Config, LineNo};

// TODO: basically everything here probablt should be moved to a python_process_info module
// (works without a pythonspy)
use crate::python_process_info::{is_python_lib, ContainsAddr, PythonProcessInfo, get_python_version, get_interpreter_address, get_threadstate_address};

#[derive(Debug, Clone)]
pub struct CoreMapRange {
    pub pathname: Option<PathBuf>,
    pub segment: goblin::elf::ProgramHeader,
}

// Defines accessors to match those in proc_maps. However, can't use the
// proc_maps trait since is private
impl CoreMapRange {
    pub fn size(&self) -> usize { self.segment.p_memsz as usize }
    pub fn start(&self) -> usize { self.segment.p_vaddr as usize }
    pub fn filename(&self) -> Option<&Path> { self.pathname.as_deref() }
    pub fn is_exec(&self) -> bool { self.segment.is_executable() }
    pub fn is_write(&self) -> bool { self.segment.is_write() }
    pub fn is_read(&self) -> bool { self.segment.is_read() }
}

impl ContainsAddr for Vec<CoreMapRange> {
    fn contains_addr(&self, addr: usize) -> bool {
        self.iter().any(|map| (addr >= map.start()) && (addr < (map.start() + map.size())))
    }
}

pub struct CoreDump {
    filename: PathBuf,
    contents: Vec<u8>,
    maps: Vec<CoreMapRange>,
}

impl CoreDump {
    pub fn new(filename: &Path) -> Result<CoreDump, Error> {
        let mut file = File::open(filename)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        let elf  = goblin::elf::Elf::parse(&contents)?;

        // TODO: no-unwrap (return an error if there are no notes)
        let notes = elf.iter_note_headers(&contents).unwrap();

        // TODO: parse out any other information we want to display here

        let mut filenames = HashMap::new();
        for note in notes {
            if let Ok(note) = note {
                if note.n_type == goblin::elf::note::NT_FILE {
                    let data = note.desc;
                    let ptrs = data.as_ptr() as * const usize;

                    let count = unsafe { *ptrs };
                    let _page_size = unsafe { *ptrs.offset(1) };

                    let string_table = &data[(std::mem::size_of::<usize>() * (2 + count * 3))..];

                    for (i, filename) in string_table.split(|chr| *chr == 0).enumerate() {
                        if i < count {
                            let i = i as isize;
                            let start = unsafe { *ptrs.offset(i * 3 + 2) };
                            let _end = unsafe { *ptrs.offset(i * 3 + 3) };
                            let _page_offset = unsafe { *ptrs.offset(i * 3 + 4) };

                            let pathname = Path::new(&OsStr::from_bytes(filename)).to_path_buf();
                            filenames.insert(start, pathname);
                        }
                    }
                }
            }
        }

        let mut maps = Vec::new();
        for ph in elf.program_headers {
            if ph.p_type == goblin::elf::program_header::PT_LOAD {
                let pathname = filenames.get(&(ph.p_vaddr as _));
                let map = CoreMapRange {pathname: pathname.cloned(), segment: ph};
                info!("map: {:016x}-{:016x} {}{}{} {}", map.start(), map.start() + map.size(),
                    if map.is_read() {'r'} else {'-'}, if map.is_write() {'w'} else {'-'}, if map.is_exec() {'x'} else {'-'},
                    map.filename().unwrap_or(&std::path::PathBuf::from("")).display());

                maps.push(map);
            }
        }

        Ok(CoreDump{filename: filename.to_owned(), contents, maps})
    }
}

impl ProcessMemory for CoreDump {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), remoteprocess::Error> {
        let start = addr as u64;
        let _end = (addr + buf.len()) as u64;

        for map in &self.maps {
            // TODO: issue is the bss addr spans multiple mmap sections - so checking the 'end'
            // here means we skip it (though works)
            // if start >= ph.p_vaddr && end <= (ph.p_vaddr + ph.p_memsz) {
            let ph = &map.segment;
            if start >= ph.p_vaddr && start <= (ph.p_vaddr + ph.p_memsz) {
                let offset = (start - ph.p_vaddr + ph.p_offset) as usize;
                buf.copy_from_slice(&self.contents[offset..(offset+buf.len())]);
                return Ok(())
            }
        }

        let io_error = std::io::Error::from_raw_os_error(libc::EFAULT);
        Err(remoteprocess::Error::IOError(io_error))
    }
}

pub struct PythonCoreDump {
    core: CoreDump,
    config: Config,
    python_info: PythonProcessInfo,
    version: Version,
    interpreter_address: usize,
    threadstate_address: usize,
}

impl PythonCoreDump {
    pub fn new(filename: &Path, config: &Config) -> Result<PythonCoreDump, Error> {

        let core = CoreDump::new(filename)?;
        let maps = &core.maps;

        // Get the python binary from the maps, and parse it
        let (python_filename, python_binary) = {
            let map = maps.iter().find(|m| m.filename().is_some() & m.is_exec()).ok_or_else(|| format_err!("Failed to get binary from coredump"))?;
            let python_filename = map.filename().unwrap();
            let python_binary = parse_binary(python_filename, map.start() as _ , map.size() as _);
            info!("Found python binary @ {}", python_filename.display());
            (python_filename.to_owned(), python_binary)
        };

        // get the libpython binary (if any) from maps
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
                    let parsed = parse_binary(filename, libpython.start() as u64, libpython.size() as u64)?;
                    libpython_binary = Some(parsed);
                }
            }
            libpython_binary
        };

        // If we have a libpython binary - we can tolerate failures on parsing the main python binary.
        let python_binary = match libpython_binary {
            None => Some(python_binary.context("Failed to parse python binary")?),
            _ => python_binary.ok(),
        };

        let python_info = PythonProcessInfo{python_binary, libpython_binary, maps: Box::new(core.maps.clone()),
            python_filename: python_filename, dockerized: false};

        let version = get_python_version(&python_info, &core).context("failed to get python version")?;
        info!("Got python version {}", version);

        let interpreter_address = get_interpreter_address(&python_info, &core, &version)?;
        info!("Found interpreter at 0x{:016x}", interpreter_address);

        // lets us figure out which thread has the GIL
        let threadstate_address = get_threadstate_address(&python_info, &version, &config)?;
        info!("found threadstate at 0x{:016x}", threadstate_address);

        Ok(PythonCoreDump{core, config: config.clone(), python_info, version, interpreter_address, threadstate_address})
    }

    pub fn get_stack(&self) -> Result<Vec<StackTrace>, Error> {
        // different versions have different layouts, check as appropriate
        Ok(match self.version {
            Version{major: 2, minor: 3..=7, ..} => self._get_stack::<v2_7_15::_is>(),
            Version{major: 3, minor: 3, ..} => self._get_stack::<v3_3_7::_is>(),
            Version{major: 3, minor: 4..=5, ..} => self._get_stack::<v3_5_5::_is>(),
            Version{major: 3, minor: 6, ..} => self._get_stack::<v3_6_6::_is>(),
            Version{major: 3, minor: 7, ..} => self._get_stack::<v3_7_0::_is>(),
            Version{major: 3, minor: 8, ..} => self._get_stack::<v3_8_0::_is>(),
            Version{major: 3, minor: 9, ..} => self._get_stack::<v3_9_5::_is>(),
            Version{major: 3, minor: 10, ..} => self._get_stack::<v3_10_0::_is>(),
            Version{major: 3, minor: 11, ..} => self._get_stack::<v3_11_0::_is>(),
            _ => Err(format_err!("Unsupported version of Python: {}", self.version))
        }?)
    }

    fn _get_stack<I: InterpreterState>(&self) -> Result<Vec<StackTrace>, Error> {
	let interp: I = self.core.copy_struct(self.interpreter_address)?;

	let mut traces = get_stack_traces(&interp, &self.core, self.threadstate_address, None)?;
	let thread_names = thread_names_from_interpreter(&interp, &self.core, &self.version).ok();

	for trace in &mut traces {
	    if let Some(ref thread_names) = thread_names {
		trace.thread_name = thread_names.get(&trace.thread_id).cloned();
	    }
	}
	Ok(traces)
    }

    pub fn print_traces(&self, traces: &Vec<StackTrace>) -> Result<(), Error> {
        // TODO: json output
        //      needs config object
        //      should change active status to 'idle'

        // TODO: show info from coredump (like program name, timestamp etC)
        println!("Core {}", style(self.core.filename.display()).bold());
        println!("Python v{}",
            style(&self.version).bold());

        for trace in traces.iter().rev() {
            let thread_id = trace.format_threadid();

            // unlike the main dump - don't show thread active status since we can't easily get that
            // from the core dump
            let status = if trace.owns_gil { format!(" (gil)") } else { "".to_owned() };
            match trace.thread_name.as_ref() {
                Some(name) => {
                    println!("Thread {}{}: \"{}\"", style(thread_id).bold().yellow(), status, name);
                }
                None => {
                    println!("Thread {}{}", style(thread_id).bold().yellow(), status);
                }
            };

            for frame in &trace.frames {
                let filename = match &frame.short_filename { Some(f) => &f, None => &frame.filename };
                if frame.line != 0 {
                    println!("    {} ({}:{})", style(&frame.name).green(), style(&filename).cyan(), style(frame.line).dim());
                } else {
                    println!("    {} ({})", style(&frame.name).green(), style(&filename).cyan());
                }
                // TODO: local variables
            }
        }

        Ok(())
    }
}


  /*  TODO:
        * pass Config object into coredump.rs
        * add flag to Config (like allow coredump instead of pid)
        * local vars
        * warn about no native functionality
        * disable compiling for non -linux
        * unittest
        * Display other core related information (timestamps ? commandline etc?)
            * requires us parsing some of the structs in elfcore NT_PRPSINFO  / NT_PRSTATUS
            * prpsinfo : program name / commandline etc / pid
            * prstatus : ?? timestamp ??

     DONE:
        * handle PID in get_stack_trace appropriately
        * split pythonprocessinfo to own file / make coredump not rely on python_spy
        * unify CoreDump  / ContainsAddr functionality
            * parse directly as elf
            * split coredump / python coredump functionality out
        * display output formatting
        * threadnames
        * GIL
            * move code from pythonspy to pythonprocessinfo (error_on_gil, get_threadstate_address
            * etc)
    */

    // TODO: output formatting
    // TODO: dispatch macro : basically call a function templatized on interpreterstate
    // version (outside of scope of this PR)
    // TODO: display other core related information (?)

/* TODO: do we still need this
                let name = match note.n_type {
                    // TODO: valide for name == "CORE"
                    goblin::elf::note::NT_FILE => "NT_FILE",  // mapped files
                    goblin::elf::note::NT_PRSTATUS => "NT_PRSTATUS",  // (prstatus structure)
                    goblin::elf::note::NT_SIGINFO => "NT_SIGINFO",  // (siginfo_t data)
                    goblin::elf::note::NT_PRPSINFO => "NT_PRPSINFO", // (prpsinfo structure)

                    // https://github.com/rust-lang/libc/blob/e4b8fd4f59a87346c870295c8125469c672998aa/src/unix/linux_like/linux/gnu/mod.rs#L939
                    // doesn't seem to be defined in goblin though?
                    2 => "NT_FPREGSET", // (floating point registers)
                    6 => "NT_AUXV", // (auxiliary vector)
                    // TODO: valid for name == "LINUX"
                    514 => "NT_X86_XSTATE",  // (x86 XSAVE extended state)
                    _ => "other"
                };

  */
                // PRSTATUS: registers/ pid /signal information  usertime/systemtime
                // https://github.com/torvalds/linux/blob/01f856ae6d0ca5ad0505b79bf2d22d7ca439b2a1/include/linux/elfcore.h#L32
                // Is there a memory layout for this anywhere ?
                // is in elfcore, probably will have to come up with our own here
                /*
                if note.n_type ==  goblin::elf::note::NT_PRSTATUS  {
                    let registers = unsafe { *(note.desc[112..].as_ptr() as * const Registers) };
                    println!("dude {} {} {:#?}", note.desc.len(), std::mem::size_of::<Registers>(), registers);
                }
                */
