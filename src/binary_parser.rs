use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::Error;
use goblin::Object;
use memmap2::Mmap;

use crate::utils::is_subrange;

pub struct BinaryInfo {
    pub symbols: HashMap<String, u64>,
    pub bss_sections: Vec<(u64, u64)>, // (addr, size) pairs
    #[allow(dead_code)]
    pub addr: u64,
    #[allow(dead_code)]
    pub size: u64,
}

impl BinaryInfo {
    #[cfg(feature = "unwind")]
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.addr && addr < (self.addr + self.size)
    }
}

/// Uses goblin to parse a binary file, returns information on symbols/bss/adjusted offset etc
pub fn parse_binary(filename: &Path, addr: u64, size: u64) -> Result<BinaryInfo, Error> {
    let offset = addr;

    let mut symbols = HashMap::new();

    // Read in the filename
    let file = File::open(filename)?;
    let buffer = unsafe { Mmap::map(&file)? };

    // Use goblin to parse the binary
    match Object::parse(&buffer)? {
        Object::Mach(mach) => {
            // Get the mach binary from the archive
            let mach = match mach {
                goblin::mach::Mach::Binary(mach) => mach,
                goblin::mach::Mach::Fat(fat) => {
                    let arch = fat
                        .iter_arches()
                        .find(|arch| match arch {
                            Ok(arch) => arch.is_64(),
                            Err(_) => false,
                        })
                        .ok_or_else(|| {
                            format_err!(
                                "Failed to find 64 bit arch in FAT archive in {}",
                                filename.display()
                            )
                        })??;
                    if !is_subrange(0, buffer.len(), arch.offset as usize, arch.size as usize) {
                        return Err(format_err!(
                            "Invalid offset/size in FAT archive in {}",
                            filename.display()
                        ));
                    }
                    let bytes = &buffer[arch.offset as usize..][..arch.size as usize];
                    goblin::mach::MachO::parse(bytes, 0)?
                }
            };

            let mut bss_sections = Vec::new();
            for segment in mach.segments.iter() {
                for (section, _) in &segment.sections()? {
                    let name = section.name()?;
                    if name == "__bss" || name == "__common" || name == "PyRuntime" {
                        if let Some(addr) = section.addr.checked_add(offset) {
                            if addr.checked_add(section.size).is_some() {
                                bss_sections.push((addr, section.size));
                            }
                        }
                    }
                }
            }

            if let Some(syms) = mach.symbols {
                for symbol in syms.iter() {
                    let (name, value) = symbol?;
                    // almost every symbol we care about starts with an extra _, remove to normalize
                    // with the entries seen on linux/windows
                    if let Some(stripped_name) = name.strip_prefix('_') {
                        symbols.insert(stripped_name.to_string(), value.n_value + offset);
                    }
                }
            }
            Ok(BinaryInfo {
                symbols,
                bss_sections,
                addr,
                size,
            })
        }

        Object::Elf(elf) => {
            let program_header = elf
                .program_headers
                .iter()
                .find(|header| {
                    header.p_type == goblin::elf::program_header::PT_LOAD
                        && header.p_flags & goblin::elf::program_header::PF_X != 0
                })
                .ok_or_else(|| {
                    format_err!(
                        "Failed to find executable PT_LOAD program header in {}",
                        filename.display()
                    )
                })?;

            // p_vaddr may be larger than the map address in case when the header has an offset and
            // the map address is relatively small. In this case we can default to 0.
            let offset = offset.saturating_sub(program_header.p_vaddr);

            let strtab = elf.shdr_strtab;
            let mut bss_sections = Vec::new();
            let mut bss_end = 0;

            for bss_header in elf
                .section_headers
                .iter()
                // filter down to things that are both NOBITS sections and are named .bss
                .filter(|header| header.sh_type == goblin::elf::section_header::SHT_NOBITS)
                .filter(|header| {
                    strtab
                        .get_at(header.sh_name)
                        .map_or(true, |name| name == ".bss" || name == ".PyRuntime")
                })
            {
                let Some(addr) = bss_header.sh_addr.checked_add(offset) else {
                    warn!("Invalid bss address in {}", filename.display());
                    continue;
                };
                let Some(end) = addr.checked_add(bss_header.sh_size) else {
                    warn!("Invalid bss size in {}", filename.display());
                    continue;
                };
                bss_sections.push((addr, bss_header.sh_size));
                bss_end = bss_end.max(end);
            }

            for sym in elf.syms.iter() {
                // Skip undefined symbols.
                if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
                    continue;
                }
                // Skip imported symbols
                if sym.is_import()
                    || (bss_end != 0
                        && sym.st_size != 0
                        && !is_subrange(0u64, bss_end, sym.st_value, sym.st_size))
                {
                    continue;
                }
                if let Some(pos) = sym.st_value.checked_add(offset) {
                    if sym.is_function() && !is_subrange(addr, size, pos, sym.st_size) {
                        continue;
                    }
                    if let Some(name) = elf.strtab.get_unsafe(sym.st_name) {
                        symbols.insert(name.to_string(), pos);
                    }
                }
            }
            for dynsym in elf.dynsyms.iter() {
                // Skip undefined symbols.
                if dynsym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
                    continue;
                }
                // Skip imported symbols
                if dynsym.is_import()
                    || (bss_end != 0
                        && dynsym.st_size != 0
                        && !is_subrange(0u64, bss_end, dynsym.st_value, dynsym.st_size))
                {
                    continue;
                }
                if let Some(pos) = dynsym.st_value.checked_add(offset) {
                    if dynsym.is_function() && !is_subrange(addr, size, pos, dynsym.st_size) {
                        continue;
                    }
                    if let Some(name) = elf.dynstrtab.get_unsafe(dynsym.st_name) {
                        symbols.insert(name.to_string(), pos);
                    }
                }
            }

            Ok(BinaryInfo {
                symbols,
                bss_sections,
                addr,
                size,
            })
        }
        Object::PE(pe) => {
            for export in pe.exports {
                if let Some(name) = export.name {
                    if let Some(addr) = offset.checked_add(export.rva as u64) {
                        symbols.insert(name.to_string(), addr);
                    }
                }
            }

            let mut bss_sections = Vec::new();
            let mut found_data = false;
            for section in pe.sections.iter() {
                let data = section.name.starts_with(b".data");
                if data {
                    found_data = true;
                }
                if data || section.name.starts_with(b"PyRuntim") {
                    if let Some(addr) = offset.checked_add(section.virtual_address as u64) {
                        if addr.checked_add(section.virtual_size as u64).is_some() {
                            bss_sections.push((addr, u64::from(section.virtual_size)))
                        }
                    }
                }
            }

            if !found_data {
                return Err(format_err!(
                    "Failed to find .data section in PE binary of {}",
                    filename.display()
                ));
            }

            Ok(BinaryInfo {
                symbols,
                bss_sections,
                addr,
                size,
            })
        }
        _ => Err(format_err!("Unhandled binary type")),
    }
}
