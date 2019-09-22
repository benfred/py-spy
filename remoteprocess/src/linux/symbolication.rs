use std::fs::File;
use std::path::Path;
use std::cell::RefCell;
use std::collections::BTreeMap;

use memmap;
use memmap::Mmap;

use object::{self, Object};
use addr2line::Context;
use goblin;
use goblin::elf::program_header::*;
use crate::{StackFrame, Error, Process, Pid };

use crate::ProcessMemory;


pub struct Symbolicator {
    binaries: BTreeMap<u64, BinaryInfo>,
    process: Process,
    pid: Pid
}

impl Symbolicator {
    pub fn new(pid: Pid) -> Result<Symbolicator, Error> {
        let process = Process::new(pid)?;
        let mut ret = Symbolicator{binaries: BTreeMap::new(), process, pid};
        ret.reload()?;
        Ok(ret)
    }

    pub fn reload(&mut self) -> Result<(), Error> {
        info!("reloading process binaries");

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
                // vsyscall region, can be ignored, but lets not keep on trying to do this
                info!("skipping {} region", filename);

                // insert a stub for [vsyscall] so that we don't continually try to load it etc
                self.binaries.insert(address_key,
                        BinaryInfo{offset: 0, address: m.start() as u64, size: m.size() as u64,
                                   filename: filename.to_string(), symbols: RefCell::new(None)});
                continue;
            };

            debug!("loading file {} 0x{:X} 0x{:X}", filename, m.start(), buffer.len());
            match goblin::Object::parse(&buffer) {
                Ok(goblin::Object::Elf(elf)) => {
                    trace!("filename {} elf {:#?}", filename, elf);

                    let program_header = elf.program_headers
                        .iter()
                        .find(|ref header| header.p_type == PT_LOAD && header.p_flags & PF_X != 0);

                    let obj_base = match program_header {
                        Some(hdr) => { m.start() as u64 - hdr.p_vaddr },
                        None => {
                            warn!("Failed to find exectuable PT_LOAD header in {}", filename);
                            continue;
                        }
                    };

                    // the map key is the end address of this filename, which lets us do a relatively efficent range
                    // based lookup of the binary
                    self.binaries.insert(address_key,
                        BinaryInfo{offset: obj_base, address: m.start() as u64, size: m.size() as u64,
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

    pub fn symbolicate(&self, addr: u64, line_info: bool, callback: &mut dyn FnMut(&StackFrame)) -> Result<(), Error> {
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

pub struct SymbolData {
    // Contains symbol info for a single binary
    ctx: Context,
    offset: u64,
    symbols: Vec<(u64, u64, String)>,
    dynamic_symbols: Vec<(u64, u64, String)>,
    filename: String
}

impl SymbolData {
    pub fn new(filename: &str, offset: u64) -> Result<SymbolData, Error> {
        info!("opening {} for symbols", filename);

        let file = File::open(filename)?;
        let map = unsafe { memmap::Mmap::map(&file)? };
        let file = match object::File::parse(&*map) {
            Ok(f) => f,
            Err(e) => {
                error!("failed to parse file for symbolication {}: {:?}", filename, e);
                // return Err(gimli::Error::OffsetOutOfBounds.into());
                return Err(Error::Other("Failed to parse file for symbolication".to_string()));
            }
        };

        let ctx = Context::new(&file)
            .map_err(|e| Error::Other(format!("Failed to get symbol context for {}: {:?}", filename, e)))?;

        let mut symbols = Vec::new();
        for (_, sym) in file.symbols() {
            if let Some(name) = sym.name() {
                symbols.push((sym.address(), sym.size(), name.to_string()));
            }
        }
        symbols.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut dynamic_symbols = Vec::new();
        for (_, sym) in file.dynamic_symbols() {
            if let Some(name) = sym.name() {
                dynamic_symbols.push((sym.address(), sym.size(), name.to_string()));
            }
        }
        dynamic_symbols.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(SymbolData{ctx, offset, dynamic_symbols, symbols, filename: filename.to_owned()})
    }

    pub fn symbolicate(&self, addr: u64, line_info: bool, callback: &mut dyn FnMut(&StackFrame)) -> Result<(), Error> {
        let mut ret = StackFrame{line:None, filename: None, function: None, addr, module: self.filename.clone()};

        // get the address before relocations
        let offset = addr - self.offset;

        // if we are being asked for line information, sue gimli addr2line to look up the debug info
        // (this is slow, and not necessary all the time which is why we are skipping)
        if line_info {
            let mut has_debug_info = false;

            // addr2line0.8 uses an older version of gimli (0.0.19) than we are using here (0.0.21),
            // this means we can't use the type of the error returned ourselves here since the
            // type alias is private. hack by re-mapping the error
            let error_handler = |e| Error::Other(format!("addr2line error: {:?}", e));

            // if we have debugging info, get the appropiate stack frames for the adresss
            let mut frames = self.ctx.find_frames(offset).map_err(error_handler)?;
            while let Some(frame) = frames.next().map_err(error_handler)? {
                has_debug_info = true;
                if let Some(func) = frame.function {
                    ret.function = Some(func.raw_name().map_err(error_handler)?.to_string());
                }
                if let Some(loc) = frame.location {
                    ret.line = loc.line;
                    if let Some(file) = loc.file.as_ref() {
                        ret.filename = Some(file.to_string());
                    }
                }
                callback(&ret);
            }

            if has_debug_info {
                return Ok(())
            }
        }

        // otherwise try getting the function name from the symbols
        if self.symbols.len() > 0 {
            let symbol = match self.symbols.binary_search_by(|sym| sym.0.cmp(&offset)) {
                Ok(i) => &self.symbols[i],
                Err(i) => &self.symbols[if i > 0 { i - 1 } else { 0 }]
            };
            if offset >= symbol.0 && offset < (symbol.0 + symbol.1) {
                ret.function = Some(symbol.2.clone());
            }
        }

        if ret.function.is_none() && self.dynamic_symbols.len() > 0 {
            let symbol = match self.dynamic_symbols.binary_search_by(|sym| sym.0.cmp(&offset)) {
                Ok(i) => &self.dynamic_symbols[i],
                Err(i) => &self.dynamic_symbols[if i > 0 { i - 1 } else { 0 }]
            };
            if offset >= symbol.0 && offset < (symbol.0 + symbol.1) {
                ret.function = Some(symbol.2.clone());
            }
        }
        callback(&ret);
        Ok(())
    }
}


// Contains info for a binary on how to unwind/symbolicate a stack trace
struct BinaryInfo {
    address: u64,
    size: u64,
    offset: u64,
    filename: String,
    symbols: RefCell<Option<Result<SymbolData, Error>>>
}

impl BinaryInfo {
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.address && addr < (self.address + self.size)
    }
}