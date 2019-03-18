use std::fs::File;
use memmap;

use object::{self, Object};
use addr2line::Context;
use gimli;
use crate::{StackFrame, Error};


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
                return Err(gimli::Error::OffsetOutOfBounds.into());
            }
        };

        let ctx = Context::new(&file)
            .map_err(|e| Error::Other(format!("Failed to get symbol context for {}: {:?}", filename, e)))?;

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

    pub fn symbolicate(&self, addr: u64, callback: &mut FnMut(&StackFrame)) -> Result<(), Error> {
        let mut ret = StackFrame{line:None, filename: None, function: None, addr, module: self.filename.clone()};

        // get the address before relocations
        let offset = addr - self.offset;
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
