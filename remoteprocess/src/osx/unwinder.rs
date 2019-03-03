use std;
use memmap;
use goblin;
use proc_maps;

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::cell::RefCell;

use super::{Error, Thread};
use goblin::error::Error as GoblinError;
use mach::port::mach_port_name_t;
use mach::structs::x86_thread_state64_t;
pub use read_process_memory::Pid;
use read_process_memory::{TryIntoProcessHandle, ProcessHandle};

use super::super::StackFrame;

use super::compact_unwind::{get_compact_unwind_info, get_dwarf_offset, compact_unwind};
use super::symbolication;
use dwarf_unwind::UnwindInfo;
use super::super::copy_struct;

pub struct Unwinder {
    pub binaries: BTreeMap<u64, SharedLibrary>,
    pub pid: Pid,
    pub process: ProcessHandle,
    pub task: mach_port_name_t,
    pub cs: symbolication::CoreSymbolication
}

impl Unwinder {
    pub fn new(pid: Pid, task: mach_port_name_t) -> Result<Unwinder, GoblinError> {
        let process = pid.try_into_process_handle()?;
        let binaries = BTreeMap::new();
        // TODO: no unwrap
        let cs = unsafe { symbolication::CoreSymbolication::new(pid) }.unwrap();
        let mut unwinder = Unwinder{binaries, pid, task, process, cs};
        unwinder.reload()?;
        Ok(unwinder)
    }

    pub fn reload(&mut self) -> Result<(), GoblinError> {
        info!("reloading binaries");
        // Get __TEXT dyld info for the process
        let dyld = proc_maps::mac_maps::get_dyld_info(self.pid)?
            .into_iter()
            .filter(|dyld| dyld.segment.segname.starts_with(&[95_i8, 95, 84, 69, 88, 84]));

        let mut loaded = 0;
        for library in dyld {
            // if we've already loaded this thing up, we're good
            let address_key = library.address as u64 + library.segment.vmsize;
            if self.binaries.contains_key(&address_key) {
                debug!("skipping {}", library.filename);
                continue;
            }

            match SharedLibrary::new(&library) {
                Ok(library) => {
                    info!("loaded shared library {:?}", library);
                    loaded += 1;
                    self.binaries.insert(address_key, library);
                }
                Err(e) =>  {
                    error!("Failed to load shared library {}: {}", library.filename, e)
                }
            };
        }
        // reload core symbolication framework too if necessary (otherwise
        // we will fail to symbolicate modules that have been loaded since this)
        if loaded > 0 {
            let cs = unsafe { symbolication::CoreSymbolication::new(self.pid) };
            if let Some(cs) = cs {
                self.cs = cs;
            }
        }

        Ok(())
    }

    pub fn get_binary(&self, pc: u64) -> Option<&SharedLibrary> {
        match self.binaries.range(pc..).next() {
            Some((_, binary)) if binary.contains(pc as usize) => Some(&binary),
            Some(_) => None,
            _ => None
        }
    }

    pub fn cursor(&self, thread: &Thread) -> Result<Cursor, std::io::Error> {
        Ok(Cursor{registers: thread.registers()?, parent: self, initial_frame: true})
    }

    pub fn symbolicate(&self, addr: u64, callback: &mut FnMut(&StackFrame)) -> Result<(), Error> {
        // Get the symbols for the current address
        let symbol = unsafe { self.cs.resolve(addr) };

        let binary = self.get_binary(addr);
        let module = binary.map_or_else(|| "?".to_owned(), |b| b.filename.clone());
        let mut function = None;
        let mut filename = None;
        let mut line = None;

        if let Some(symbol) = symbol {
            if let Some(name) = symbol.name() {
                if let Ok(name) = name.to_str() {
                    function = Some(name.to_owned());
                }
            }
            if let Some(name) = symbol.filename() {
                if let Ok(name) = name.to_str() {
                    filename = Some(name.to_owned());
                }
            }
            line = Some(symbol.lineno as u64);
        }
        callback(&StackFrame{function, filename, line, module, addr});
        Ok(())
    }
}

#[derive(Debug)]
pub struct SharedLibrary {
    pub filename: String,
    pub address: usize,
    pub size: usize,
    pub mh_offset: usize,
    pub buffer: memmap::Mmap,
    pub unwind_info: Option<OffsetRange>,
    pub eh_frame: Option<OffsetRange>,
    pub dwarf_info: RefCell<Option<UnwindInfo>>
}

impl SharedLibrary {
    fn new(library: &proc_maps::mac_maps::DyldInfo) -> Result<SharedLibrary, GoblinError> {
        debug!("loading file {} 0x{:X}", library.filename, library.address);
        let file = File::open(Path::new(&library.filename))?;
        let buffer = unsafe { memmap::Mmap::map(&file)? };

        // get the __eh_frame and __unwind_info sections from the mach binary
        let mut eh_frame: Option<OffsetRange> = None;
        let mut unwind_info: Option<OffsetRange> = None;
        let mut mh_offset = 0;
        match goblin::Object::parse(&buffer)? {
            goblin::Object::Mach(mach) => {
                let macho = match mach {
                    goblin::mach::Mach::Binary(macho) => macho,
                    goblin::mach::Mach::Fat(fat) => {
                        let arch = fat.iter_arches().find(|arch|
                            match arch {
                                Ok(arch) => arch.is_64(),
                                Err(_) => false
                            }
                        ).expect("Failed to find 64 bit arch in FAT archive")?;
                        debug!("got 64bit archive from fat archive @ {} ({} bytes)", arch.offset, arch.size);
                        mh_offset = arch.offset as usize;
                        let bytes = &buffer[arch.offset as usize..][..arch.size as usize];
                        goblin::mach::MachO::parse(bytes, 0)?
                    }
                };

                // Get the text segment from the binary
                let text = macho.segments
                    .iter()
                    .find(|s| { s.name().unwrap_or("") == "__TEXT" })
                    .ok_or_else(|| GoblinError::Malformed(format!("Failed to find __TEXT section in {}", library.filename)))?;

                // Get the unwind_info and eh_frame sections
                let sections = text.sections()?;
                for (section, _) in sections.iter() {
                    match section.name() {
                        Ok("__eh_frame") => { eh_frame = Some(OffsetRange::from(section, mh_offset)) },
                        Ok("__unwind_info") => { unwind_info = Some(OffsetRange::from(section, mh_offset)) },
                        _ => {}
                    }
                }
            }
            _ => {
                return Err(GoblinError::Malformed(format!("Shared library {} is not a mach binary", library.filename)))?;
            }
        }

        Ok(SharedLibrary{filename: library.filename.clone(),
                        address: library.address,
                        size: library.segment.vmsize as usize,
                        buffer,
                        mh_offset,
                        unwind_info,
                        eh_frame,
                        dwarf_info: RefCell::new(None)})
    }

    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.address && addr < (self.address + self.size)
    }
}


pub struct Cursor<'a> {
    registers: x86_thread_state64_t,
    parent: &'a Unwinder,
    initial_frame: bool
}

impl<'a> Cursor<'a> {
    fn unwind(&mut self) -> Result<Option<u64>, Error> {
        let process = self.parent.process;
        if self.initial_frame {
            self.initial_frame = false;
            return Ok(Some(self.registers.__rip));
        }

        let check = |rip| {
            match rip {
                0...0x1000 => None,
                _ => Some(rip)
            }
        };

        let pc = self.registers.__rip - 1;
        let binary = match self.parent.get_binary(pc) {
            Some(binary) => binary,
            None => {
                // this seems to happen legitimately sometimes (in firefox, as confirmed by lldb also not knowing the
                // binary for the same address). BUT could also mean that we need to reload the binaries
                // return an error and attempt reloading just in case
                return Err(Error::NoBinaryForAddress(pc));
            }
        };

        // Try doing a compact unwind first
        if let Some(unwind_info) = &binary.unwind_info {
            let unwind_buffer = &binary.buffer[unwind_info.offset as usize..][..unwind_info.size as usize];
            let info = get_compact_unwind_info(unwind_buffer, binary.address as u64, pc)?;
            if info.encoding == 0 {
                debug!("frameless unwind fallback 0x{:016x}. registers {:?}", pc, self.registers);

                let old_rip = self.registers.__rip;

                // If no encoding was given, assuming a frameless unwind with no stack size.
                // I can't find any documentation on this, but this seems like it works
                self.registers.__rip = copy_struct(self.registers.__rsp as usize, &process)?;
                self.registers.__rsp += 8;

                // except it doesn't always work =( hack (TODO: figure out what to do in this case!)
                if old_rip == self.registers.__rip {
                    return Ok(None);
                }

                return Ok(check(self.registers.__rip));


            } else if let Some(offset) = get_dwarf_offset(info.encoding) {
                // we could do use the offset here to speed up the FDE lookup (and avoid building fde lookup table),
                // but in the meantime lets fallback to the dwarf unwinding code below
                // looking at the gimli code, and I don't see any easy way of leveraging this offset without exposing
                // a private function =(. TODO: come back to this (very low priority)
                debug!("compact dwarf unwind (offset = {})", offset);

            } else if info.encoding != 0 {
                compact_unwind(&info, &mut self.registers, &process)?;
                return Ok(check(self.registers.__rip));
            }
        }

        if let Some(eh_frame_range) = &binary.eh_frame {
            debug!("dwarf unwind 0x{:016x}", self.registers.__rip);

            let mut dwarf_info = binary.dwarf_info.borrow_mut();
            if dwarf_info.is_none() {
                let eh_frame = &binary.buffer[eh_frame_range.offset as usize..][..eh_frame_range.size as usize];
                let eh_frame_address = binary.address as u64 + eh_frame_range.offset as u64 - binary.mh_offset as u64;
                *dwarf_info = Some(UnwindInfo::new(eh_frame, eh_frame_address)?);
            }

            let unwinder = dwarf_info.as_ref().unwrap();
            if !unwinder.unwind(&mut self.registers, &process)? {
                return Ok(None);
            }
            return Ok(check(self.registers.__rip));
        }

        // TODO: return an error here?
        info!("failed to do a compact unwind, and there is no dwarf debugging info present");
        Ok(None)
    }

    pub fn ip(&self) -> u64 { self.registers.__rip }
    pub fn sp(&self) -> u64 { self.registers.__rsp }
    pub fn bp(&self) -> u64 { self.registers.__rbp }
    pub fn bx(&self) -> u64 { self.registers.__rbx }
}

impl<'a> Iterator for Cursor<'a> {
    type Item = Result<u64, Error>;

    fn next(&mut self) -> Option<Result<u64, Error>> {
        match self.unwind() {
            Ok(Some(addr)) => Some(Ok(addr)),
            Err(e) => Some(Err(e)),
            Ok(None) => None,
        }
    }
}

// TODO: use std::ops::Range rather than define our own?
#[derive(Debug)]
pub struct OffsetRange {
    pub offset: usize,
    pub size: usize
}

impl OffsetRange {
    pub fn from(section: &goblin::mach::segment::Section, offset: usize) -> OffsetRange {
        OffsetRange{offset: section.offset as usize + offset, size: section.size as usize}
    }
}
