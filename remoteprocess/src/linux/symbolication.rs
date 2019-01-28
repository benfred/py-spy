use std::fs::File;
use memmap;

use fallible_iterator::FallibleIterator;
use object::{self, Object};
use addr2line::Context;
use gimli;
use super::super::StackFrame;

type SymbolContext = Context<gimli::EndianRcSlice<gimli::RunTimeEndian>>;

pub struct SymbolData {
    // Contains symbol info for a single binary
    ctx: SymbolContext,
    offset: u64,
    symbols: Vec<(u64, u64, String)>,
    dynamic_symbols: Vec<(u64, u64, String)>,
    filename: String
}

impl SymbolData {
    pub fn new(filename: &str, offset: u64) -> gimli::Result<SymbolData> {
        info!("opening {} for symbols", filename);

        // TODO: this object API still relies on goblin v0.15 - which
        // has some problems parsing some things. TODO: deprecate this
        let file = File::open(filename)?;
        let map = unsafe { memmap::Mmap::map(&file)? };
        let file = match object::File::parse(&*map) {
            Ok(f) => f,
            Err(e) => {
                error!("failed to parse file for symbolication {}: {:?}", filename, e);
                return Err(gimli::Error::OffsetOutOfBounds{});
            }
        };
        let ctx = Context::new(&file)?;

        let mut symbols = Vec::new();
        for sym in file.symbols() {
            if let Some(name) = sym.name() {
                symbols.push((sym.address(), sym.size(), name.to_string()));
            }
        }
        symbols.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mut dynamic_symbols = Vec::new();
        for sym in file.dynamic_symbols() {
            if let Some(name) = sym.name() {
                dynamic_symbols.push((sym.address(), sym.size(), name.to_string()));
            }
        }
        dynamic_symbols.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Ok(SymbolData{ctx, offset, dynamic_symbols, symbols, filename: filename.to_owned()})
    }

    pub fn symbolicate(&self, addr: u64, callback: &mut FnMut(&StackFrame)) -> gimli::Result<()> {
        let mut ret = StackFrame{line:None, filename: None, function: None, addr, module: self.filename.clone()};

        // get the address before relocations
        let offset = addr - self.offset;
        let mut has_debug_info = false;

        // if we have debugging info, get the appropiate stack frames for the adresss
        let mut frames = self.ctx.find_frames(offset)?.enumerate();
        while let Some((_, frame)) = frames.next()? {
            has_debug_info = true;
            if let Some(func) = frame.function {
                ret.function = Some(func.raw_name()?.to_string());
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
