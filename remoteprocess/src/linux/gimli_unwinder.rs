use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::BTreeMap;

use goblin::Object;
use memmap::Mmap;
use proc_maps;

use gimli::{EhFrame, BaseAddresses, Pointer, NativeEndian, EhFrameHdr};
use goblin::elf::program_header::*;

use gimli::EndianRcSlice;
type RcReader = EndianRcSlice<NativeEndian>;

use super::super::{ProcessMemory, Error};
use crate::dwarf_unwind::{UnwindInfo, Registers};

use crate::linux::symbolication::{SymbolData};
use super::super::StackFrame;
use super::{Pid, Thread, Process};

pub struct Unwinder {
    binaries: BTreeMap<u64, BinaryInfo>,
    process: Process,
    pid: Pid
}

pub struct Cursor<'a> {
    registers: Registers,
    parent: &'a Unwinder,
    initial_frame: bool,
}

impl Unwinder {
    pub fn new(pid: Pid) -> Result<Unwinder, Error> {
        let process = Process::new(pid)?;
        let mut ret = Unwinder{binaries: BTreeMap::new(), process, pid};
        ret.reload()?;
        Ok(ret)
    }

    pub fn reload(&mut self) -> Result<(), Error> {
        // Get shared libraries from virtual memory mapped files
        let maps = &proc_maps::get_process_maps(self.pid)?;
        let shared_maps = maps.iter().filter(|m| m.is_exec() && !m.is_write() && m.is_read());

        // Open them up and parse etc
        for m in shared_maps {
            // Get the filename if it exists from the map
            let filename = match m.filename() {
                Some(f) => f,
                None => continue
            };

            // TODO: probably also want to check if the filename/size is the same
            let address_key = (m.start() + m.size()) as u64;
            if self.binaries.contains_key(&address_key) {
                debug!("skipping {}", filename);
                continue;
            }
            info!("loading debug info from {}", filename);

            // Memory-map the file, special casing [vdso] regions
            let file;
            let mmapped_file;
            let vdso_data;

            let buffer = if Path::new(filename).exists() {
                file = File::open(Path::new(filename))?;
                mmapped_file = unsafe { Mmap::map(&file)? };
                &mmapped_file[..]
            } else if filename != "[vsyscall]" {
                // if the filename doesn't exist, its' almost certainly the vdso section
                // read from the the target processses memory
                vdso_data = self.process.copy(m.start(), m.size())?;
                &vdso_data
            } else {
                // vsyscall region, can be ignored
                debug!("skipping {} region", filename);
                continue;
            };

            debug!("loading file {} 0x{:X} 0x{:X}", filename, m.start(), buffer.len());
            match Object::parse(&buffer) {
                Ok(Object::Elf(elf)) => {
                    trace!("filename {} elf {:#?}", filename, elf);
                    // Get the base address of everything here
                    let program_header = elf.program_headers
                        .iter()
                        .find(|ref header| header.p_type == PT_LOAD && header.p_flags & PF_X != 0);

                    let obj_base = match program_header {
                        Some(hdr) => { m.start() as u64 - hdr.p_vaddr },
                        None => { warn!("Failed to find exectuable PT_LOAD header in {}", filename); continue; }
                    };

                    // get the eh_frame_hdr from the program headers
                    let mut eh_frame_hdr_addr;
                    let eh_frame_hdr =  match elf.program_headers.iter().find(|x| x.p_type == PT_GNU_EH_FRAME) {
                        Some(hdr) => {
                            eh_frame_hdr_addr = obj_base + hdr.p_vaddr;
                            let data = Rc::from(&buffer[hdr.p_offset as usize..][..hdr.p_filesz as usize]);
                            let bases = BaseAddresses::default().set_eh_frame_hdr(eh_frame_hdr_addr);
                            match EhFrameHdr::from(RcReader::new(data, NativeEndian)).parse(&bases, 8) {
                                Ok(hdr) => hdr,
                                Err(e) => {
                                    warn!("Failed to load eh_frame_hdr section from {:?}: {} - hdr {:#?}", filename, e, hdr);
                                    continue;
                                }
                            }
                        }
                        None => {
                            warn!("Failed to find eh_frame_hdr section in {}", filename);
                            continue;
                        }
                    };

                    let eh_frame_addr = match eh_frame_hdr.eh_frame_ptr() {
                        Pointer::Direct(x) => x,
                        Pointer::Indirect(x) => { self.process.copy_struct(x as usize)? }
                    };

                    // get the appropiate eh_frame section from the section_headers and load it up with gimli
                    let eh_frame = match elf.section_headers.iter().filter(|x| x.sh_addr == eh_frame_addr - obj_base).next() {
                        Some(hdr) => {
                            debug!("Got eh_frame hdr {:?} from {}", hdr, filename);
                            let data = Rc::from(&buffer[hdr.sh_offset as usize..][..hdr.sh_size as usize]);
                            EhFrame::from(RcReader::new(data, NativeEndian))
                        }
                        None => {
                            // TODO: we could build up a lookup table of the FDE's from the eh_frame section here
                            warn!("Failed to find eh_frame section in {} (expected at {:016x})", filename, eh_frame_addr);
                            continue;
                        }
                    };

                    let bases = BaseAddresses::default()
                        .set_eh_frame(eh_frame_addr)
                        .set_eh_frame_hdr(eh_frame_hdr_addr);

                    let unwind_info = UnwindInfo{eh_frame_hdr, eh_frame, bases};

                    // the map key is the end address of this filename, which lets us do a relatively efficent range
                    // based lookup of the binary
                    self.binaries.insert(address_key,
                        BinaryInfo{unwind_info, offset: obj_base, address: m.start() as u64, size: m.size() as u64,
                                   filename: filename.to_string(), symbols: RefCell::new(None)});
                },
                Ok(_) => {
                    warn!("unknown binary type for {}", filename);
                    continue;
                }
                Err(e) => {
                    warn!("Failed to parse {}: {:?}", filename, e);
                    continue;
                }
            }
        }
        Ok(())
    }

    pub fn cursor(&self, thread: &Thread) -> Result<Cursor, Error> {
        Ok(Cursor{registers: thread.registers()?, parent: self, initial_frame: true})
    }

    pub fn symbolicate(&self, addr: u64, line_info: bool, callback: &mut FnMut(&StackFrame)) -> Result<(), Error> {
        let binary = match self.get_binary(addr) {
            Some(binary) => binary,
            None => {
                return Err(Error::NoBinaryForAddress(addr));
            }
        };
        if binary.filename != "[vdso]" {
            let mut symbols = binary.symbols.borrow_mut();
            if symbols.is_none() {
                info!("loading symbols from {}", binary.filename);
                *symbols = Some(SymbolData::new(&binary.filename, binary.offset));
            }
            match symbols.as_ref() {
                Some(Ok(symbols)) => symbols.symbolicate(addr, line_info, callback),
                _ => {
                    // we probably failed to load the symbols (maybe goblin v0.15 dependency causing error
                    // in gimli/object crate). Rather than fail add a stub
                    callback(&StackFrame{line: None, addr, function: None, filename: None, module: binary.filename.clone()});
                    Ok(())
                }
            }
        } else {
            // TODO: allow symbolication code to access vdso data
            callback(&StackFrame{line: None, addr, function: None, filename: None, module: binary.filename.clone()});
            Ok(())
        }
    }

    fn get_binary(&self, addr: u64) -> Option<&BinaryInfo> {
        match self.binaries.range(addr..).next() {
            Some((_, binary)) if binary.contains(addr) => Some(&binary),
            Some(_) => None,
            _ => None
        }
    }
}

impl<'a> Cursor<'a> {
    pub fn ip(&self) -> u64 { self.registers.rip }
    pub fn sp(&self) -> u64 { self.registers.rsp }
    pub fn bp(&self) -> u64 { self.registers.rbp }
    pub fn bx(&self) -> u64 { self.registers.rbx }
}


impl<'a> Iterator for Cursor<'a> {
    type Item = Result<u64, Error>;

    fn next(&mut self) -> Option<Result<u64, Error>> {
        if self.initial_frame {
            self.initial_frame = false;
            return Some(Ok(self.registers.rip));
        }

        if self.registers.rip <= 0x1000 {
            return None;
        }

        // Otherwise get the binary for the current instruction
        let pc = self.registers.rip - 1;
        let binary = match self.parent.get_binary(pc) {
            Some(binary) => binary,
            None => {
                return Some(Err(Error::NoBinaryForAddress(pc)));
            }
        };

        let mut old_reg = self.registers.clone();

        match binary.unwind_info.unwind(&mut self.registers, &self.parent.process) {
            Ok(true) => {},
            Ok(false) => return None,
            Err(e)  => return Some(Err(Error::from(e))),
        };

        // if the frame pointer and instruction pointer haven't updated, we're also done
        // (discounting SP which will almost always update each unwind)
        old_reg.rsp = self.registers.rsp;
        if old_reg == self.registers {
            return None;
        }

        Some(Ok(self.registers.rip))
    }
}

// Contains info for a binary on how to unwind/symbolicate a stack trace
struct BinaryInfo {
    address: u64,
    size: u64,
    offset: u64,
    filename: String,
    unwind_info: UnwindInfo,
    symbols: RefCell<Option<Result<SymbolData, Error>>>
}

impl BinaryInfo {
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.address && addr < (self.address + self.size)
    }
}
