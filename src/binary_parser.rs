use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::Error;
use goblin::Object;
use memmap::Mmap;

pub struct BinaryInfo {
    pub filename: std::path::PathBuf,
    pub symbols: HashMap<String, u64>,
    pub bss_addr: u64,
    pub bss_size: u64,
    pub offset: u64,
    pub addr: u64,
    pub size: u64,
}

impl BinaryInfo {
    #[cfg(unwind)]
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
                    let bytes = &buffer[arch.offset as usize..][..arch.size as usize];
                    goblin::mach::MachO::parse(bytes, 0)?
                }
            };

            let mut bss_addr = 0;
            let mut bss_size = 0;
            for segment in mach.segments.iter() {
                for (section, _) in &segment.sections()? {
                    if section.name()? == "__bss" {
                        bss_addr = section.addr + offset;
                        bss_size = section.size;
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
                filename: filename.to_owned(),
                symbols,
                bss_addr,
                bss_size,
                offset,
                addr,
                size,
            })
        }

        Object::Elf(elf) => {
            let strtab = elf.shdr_strtab;
            let bss_header = elf
                .section_headers
                .iter()
                // filter down to things that are both NOBITS sections and are named .bss
                .filter(|header| header.sh_type == goblin::elf::section_header::SHT_NOBITS)
                .filter(|header| {
                    strtab
                        .get_at(header.sh_name)
                        .map_or(true, |name| name == ".bss")
                })
                // if we have multiple sections here, take the largest
                .max_by_key(|header| header.sh_size)
                .ok_or_else(|| {
                    format_err!(
                        "Failed to find BSS section header in {}",
                        filename.display()
                    )
                })?;

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

            for sym in elf.syms.iter() {
                // Only count defined symbols.
                if sym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
                    continue;
                }
                let name = elf.strtab[sym.st_name].to_string();
                symbols.insert(name, sym.st_value + offset);
            }
            for dynsym in elf.dynsyms.iter() {
                // Only count defined symbols.
                if dynsym.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
                    continue;
                }
                let name = elf.dynstrtab[dynsym.st_name].to_string();
                symbols.insert(name, dynsym.st_value + offset);
            }
            Ok(BinaryInfo {
                filename: filename.to_owned(),
                symbols,
                bss_addr: bss_header.sh_addr + offset,
                bss_size: bss_header.sh_size,
                offset,
                addr,
                size,
            })
        }
        Object::PE(pe) => {
            for export in pe.exports {
                if let Some(name) = export.name {
                    if let Some(export_offset) = export.offset {
                        symbols.insert(name.to_string(), export_offset as u64 + offset);
                    }
                }
            }

            pe.sections
                .iter()
                .find(|section| section.name.starts_with(b".data"))
                .ok_or_else(|| {
                    format_err!(
                        "Failed to find .data section in PE binary of {}",
                        filename.display()
                    )
                })
                .map(|data_section| {
                    let bss_addr = u64::from(data_section.virtual_address) + offset;
                    let bss_size = u64::from(data_section.virtual_size);

                    BinaryInfo {
                        filename: filename.to_owned(),
                        symbols,
                        bss_addr,
                        bss_size,
                        offset,
                        addr,
                        size,
                    }
                })
        }
        _ => Err(format_err!("Unhandled binary type")),
    }
}
