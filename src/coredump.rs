use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Error, Result};
use console::style;
use log::info;
use remoteprocess::ProcessMemory;

use crate::binary_parser::{parse_binary, BinaryInfo};
use crate::config::Config;
use crate::dump::print_trace;
use crate::python_bindings::{
    v2_7_15, v3_10_0, v3_11_0, v3_3_7, v3_5_5, v3_6_6, v3_7_0, v3_8_0, v3_9_5,
};
use crate::python_data_access::format_variable;
use crate::python_interpreters::InterpreterState;
use crate::python_process_info::{
    get_interpreter_address, get_python_version, get_threadstate_address, is_python_lib,
    ContainsAddr, PythonProcessInfo,
};
use crate::python_threading::thread_names_from_interpreter;
use crate::stack_trace::{get_stack_traces, StackTrace};
use crate::version::Version;

#[derive(Debug, Clone)]
pub struct CoreMapRange {
    pub pathname: Option<PathBuf>,
    pub segment: goblin::elf::ProgramHeader,
}

// Defines accessors to match those in proc_maps. However, can't use the
// proc_maps trait since is private
impl CoreMapRange {
    pub fn size(&self) -> usize {
        self.segment.p_memsz as usize
    }
    pub fn start(&self) -> usize {
        self.segment.p_vaddr as usize
    }
    pub fn filename(&self) -> Option<&Path> {
        self.pathname.as_deref()
    }
    pub fn is_exec(&self) -> bool {
        self.segment.is_executable()
    }
    pub fn is_write(&self) -> bool {
        self.segment.is_write()
    }
    pub fn is_read(&self) -> bool {
        self.segment.is_read()
    }
}

impl ContainsAddr for Vec<CoreMapRange> {
    fn contains_addr(&self, addr: usize) -> bool {
        self.iter()
            .any(|map| (addr >= map.start()) && (addr < (map.start() + map.size())))
    }
}

pub struct CoreDump {
    filename: PathBuf,
    contents: Vec<u8>,
    maps: Vec<CoreMapRange>,
    psinfo: Option<elfcore::elf_prpsinfo>,
    status: Vec<elfcore::elf_prstatus>,
}

impl CoreDump {
    pub fn new<P: AsRef<Path>>(filename: P) -> Result<CoreDump, Error> {
        let filename = filename.as_ref();
        let mut file = File::open(filename)?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)?;
        let elf = goblin::elf::Elf::parse(&contents)?;

        let notes = elf
            .iter_note_headers(&contents)
            .ok_or_else(|| format_err!("no note segment found"))?;

        let mut filenames = HashMap::new();
        let mut psinfo = None;
        let mut status = Vec::new();
        for note in notes.flatten() {
            if note.n_type == goblin::elf::note::NT_PRPSINFO {
                psinfo = Some(unsafe { *(note.desc.as_ptr() as *const elfcore::elf_prpsinfo) });
            } else if note.n_type == goblin::elf::note::NT_PRSTATUS {
                let thread_status =
                    unsafe { *(note.desc.as_ptr() as *const elfcore::elf_prstatus) };
                status.push(thread_status);
            } else if note.n_type == goblin::elf::note::NT_FILE {
                let data = note.desc;
                let ptrs = data.as_ptr() as *const usize;

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

        let mut maps = Vec::new();
        for ph in elf.program_headers {
            if ph.p_type == goblin::elf::program_header::PT_LOAD {
                let pathname = filenames.get(&(ph.p_vaddr as _));
                let map = CoreMapRange {
                    pathname: pathname.cloned(),
                    segment: ph,
                };
                info!(
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

                maps.push(map);
            }
        }

        Ok(CoreDump {
            filename: filename.to_owned(),
            contents,
            maps,
            psinfo,
            status,
        })
    }
}

impl ProcessMemory for CoreDump {
    fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), remoteprocess::Error> {
        let start = addr as u64;
        let _end = (addr + buf.len()) as u64;

        for map in &self.maps {
            // TODO: one issue here is the bss addr spans multiple mmap segments - so checking the 'end'
            // here means we skip it. Instead we're just checking if the start address exists in
            // the segment
            let ph = &map.segment;
            if start >= ph.p_vaddr && start <= (ph.p_vaddr + ph.p_memsz) {
                let offset = (start - ph.p_vaddr + ph.p_offset) as usize;
                buf.copy_from_slice(&self.contents[offset..(offset + buf.len())]);
                return Ok(());
            }
        }

        let io_error = std::io::Error::from_raw_os_error(libc::EFAULT);
        Err(remoteprocess::Error::IOError(io_error))
    }
}

pub struct PythonCoreDump {
    core: CoreDump,
    version: Version,
    interpreter_address: usize,
    threadstate_address: usize,
}

impl PythonCoreDump {
    pub fn new<P: AsRef<Path>>(filename: P) -> Result<PythonCoreDump, Error> {
        let core = CoreDump::new(filename)?;
        let maps = &core.maps;

        // Get the python binary from the maps, and parse it
        let (python_filename, python_binary) = {
            let map = maps
                .iter()
                .find(|m| m.filename().is_some() & m.is_exec())
                .ok_or_else(|| format_err!("Failed to get binary from coredump"))?;
            let python_filename = map.filename().unwrap();
            let python_binary = parse_binary(python_filename, map.start() as _, map.size() as _);
            info!("Found python binary @ {}", python_filename.display());
            (python_filename.to_owned(), python_binary)
        };

        // get the libpython binary (if any) from maps
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
                    let parsed =
                        parse_binary(filename, libpython.start() as u64, libpython.size() as u64)?;
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

        let python_info = PythonProcessInfo {
            python_binary,
            libpython_binary,
            maps: Box::new(core.maps.clone()),
            python_filename,
            dockerized: false,
        };

        let version =
            get_python_version(&python_info, &core).context("failed to get python version")?;
        info!("Got python version {}", version);

        let interpreter_address = get_interpreter_address(&python_info, &core, &version)?;
        info!("Found interpreter at 0x{:016x}", interpreter_address);

        // lets us figure out which thread has the GIL
        let config = Config::default();
        let threadstate_address = get_threadstate_address(&python_info, &version, &config)?;
        info!("found threadstate at 0x{:016x}", threadstate_address);

        Ok(PythonCoreDump {
            core,
            version,
            interpreter_address,
            threadstate_address,
        })
    }

    pub fn get_stack(&self, config: &Config) -> Result<Vec<StackTrace>, Error> {
        if config.native {
            return Err(format_err!(
                "Native unwinding isn't yet supported with coredumps"
            ));
        }

        if config.subprocesses {
            return Err(format_err!(
                "Subprocesses can't be used for getting stacktraces from coredumps"
            ));
        }

        // different versions have different layouts, check as appropriate
        match self.version {
            Version {
                major: 2,
                minor: 3..=7,
                ..
            } => self._get_stack::<v2_7_15::_is>(config),
            Version {
                major: 3, minor: 3, ..
            } => self._get_stack::<v3_3_7::_is>(config),
            Version {
                major: 3,
                minor: 4..=5,
                ..
            } => self._get_stack::<v3_5_5::_is>(config),
            Version {
                major: 3, minor: 6, ..
            } => self._get_stack::<v3_6_6::_is>(config),
            Version {
                major: 3, minor: 7, ..
            } => self._get_stack::<v3_7_0::_is>(config),
            Version {
                major: 3, minor: 8, ..
            } => self._get_stack::<v3_8_0::_is>(config),
            Version {
                major: 3, minor: 9, ..
            } => self._get_stack::<v3_9_5::_is>(config),
            Version {
                major: 3,
                minor: 10,
                ..
            } => self._get_stack::<v3_10_0::_is>(config),
            Version {
                major: 3,
                minor: 11,
                ..
            } => self._get_stack::<v3_11_0::_is>(config),
            _ => Err(format_err!(
                "Unsupported version of Python: {}",
                self.version
            )),
        }
    }

    fn _get_stack<I: InterpreterState>(&self, config: &Config) -> Result<Vec<StackTrace>, Error> {
        let interp: I = self.core.copy_struct(self.interpreter_address)?;

        let mut traces =
            get_stack_traces(&interp, &self.core, self.threadstate_address, Some(config))?;
        let thread_names = thread_names_from_interpreter(&interp, &self.core, &self.version).ok();

        for trace in &mut traces {
            if let Some(ref thread_names) = thread_names {
                trace.thread_name = thread_names.get(&trace.thread_id).cloned();
            }

            for frame in &mut trace.frames {
                if let Some(locals) = frame.locals.as_mut() {
                    let max_length = (128 * config.dump_locals) as isize;
                    for local in locals {
                        let repr = format_variable::<I, CoreDump>(
                            &self.core,
                            &self.version,
                            local.addr,
                            max_length,
                        );
                        local.repr = Some(repr.unwrap_or_else(|_| "?".to_owned()));
                    }
                }
            }
        }
        Ok(traces)
    }

    pub fn print_traces(&self, traces: &Vec<StackTrace>, config: &Config) -> Result<(), Error> {
        if config.dump_json {
            println!("{}", serde_json::to_string_pretty(&traces)?);
            return Ok(());
        }

        if let Some(status) = self.core.status.first() {
            println!(
                "Signal {}: {}",
                style(status.pr_cursig).bold().yellow(),
                self.core.filename.display()
            );
        }

        if let Some(psinfo) = self.core.psinfo {
            println!(
                "Process {}: {}",
                style(psinfo.pr_pid).bold().yellow(),
                OsStr::from_bytes(&psinfo.pr_psargs).to_string_lossy()
            );
        }
        println!("Python v{}", style(&self.version).bold());
        println!();
        for trace in traces.iter().rev() {
            print_trace(trace, false);
        }
        Ok(())
    }
}

mod elfcore {
    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct elf_siginfo {
        pub si_signo: ::std::os::raw::c_int,
        pub si_code: ::std::os::raw::c_int,
        pub si_errno: ::std::os::raw::c_int,
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct timeval {
        pub tv_sec: ::std::os::raw::c_long,
        pub tv_usec: ::std::os::raw::c_long,
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct elf_prstatus {
        pub pr_info: elf_siginfo,
        pub pr_cursig: ::std::os::raw::c_short,
        pub pr_sigpend: ::std::os::raw::c_ulong,
        pub pr_sighold: ::std::os::raw::c_ulong,
        pub pr_pid: ::std::os::raw::c_int,
        pub pr_ppid: ::std::os::raw::c_int,
        pub pr_pgrp: ::std::os::raw::c_int,
        pub pr_sid: ::std::os::raw::c_int,
        pub pr_utime: timeval,
        pub pr_stime: timeval,
        pub pr_cutime: timeval,
        pub pr_cstime: timeval,
        // TODO: has registers next for thread next - don't need them right now, but if we want to do
        // unwinding we will
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct elf_prpsinfo {
        pub pr_state: ::std::os::raw::c_char,
        pub pr_sname: ::std::os::raw::c_char,
        pub pr_zomb: ::std::os::raw::c_char,
        pub pr_nice: ::std::os::raw::c_char,
        pub pr_flag: ::std::os::raw::c_ulong,
        pub pr_uid: ::std::os::raw::c_uint,
        pub pr_gid: ::std::os::raw::c_uint,
        pub pr_pid: ::std::os::raw::c_int,
        pub pr_ppid: ::std::os::raw::c_int,
        pub pr_pgrp: ::std::os::raw::c_int,
        pub pr_sid: ::std::os::raw::c_int,
        pub pr_fname: [::std::os::raw::c_uchar; 16usize],
        pub pr_psargs: [::std::os::raw::c_uchar; 80usize],
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use py_spy_testdata::get_coredump_path;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_coredump() {
        // we won't have the python binary for the core dump here,
        // so we can't (yet) figure out the interpreter address & version.
        // Manually specify here to test out instead
        let core = CoreDump::new(&get_coredump_path("python_3_9_threads")).unwrap();
        let version = Version {
            major: 3,
            minor: 9,
            patch: 13,
            release_flags: "".to_owned(),
            build_metadata: None,
        };
        let python_core = PythonCoreDump {
            core,
            version,
            interpreter_address: 0x000055a8293dbe20,
            threadstate_address: 0x000055a82745fe18,
        };

        let config = Config::default();
        let traces = python_core.get_stack(&config).unwrap();

        // should have two threads
        assert_eq!(traces.len(), 2);

        let main_thread = &traces[1];
        assert_eq!(main_thread.frames.len(), 1);
        assert_eq!(main_thread.frames[0].name, "<module>");
        assert_eq!(main_thread.thread_name, Some("MainThread".to_owned()));

        let child_thread = &traces[0];
        assert_eq!(child_thread.frames.len(), 5);
        assert_eq!(child_thread.frames[0].name, "dump_sum");
        assert_eq!(child_thread.frames[0].line, 16);
        assert_eq!(child_thread.thread_name, Some("child_thread".to_owned()));
    }
}
