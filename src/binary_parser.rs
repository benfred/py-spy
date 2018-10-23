
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use goblin;
use goblin::Object;
use goblin::error::Error as GoblinError;
use memmap::Mmap;

pub struct BinaryInfo {
    pub symbols: HashMap<String, u64>,
    pub bss_addr: u64,
    pub bss_size: u64,
    pub offset: u64
}

/// Uses goblin to parse a binary file, returns information on symbols/bss/adjusted offset etc
pub fn parse_binary(filename: &str, offset: u64) -> Result<BinaryInfo, GoblinError> {
    let mut symbols = HashMap::new();

    // Read in the filename
    let file = File::open(Path::new(filename))?;
    let buffer = unsafe { Mmap::map(&file)? };

    // Use goblin to parse the binary
    match Object::parse(&buffer)? {
        Object::Mach(mach) => {
            // Get the mach binary from the archive
            let mach = match mach {
                goblin::mach::Mach::Binary(mach) => mach,
                goblin::mach::Mach::Fat(fat) => {
                    let arch = fat.iter_arches().find(|arch|
                        match arch {
                            Ok(arch) => arch.is_64(),
                            Err(_) => false
                        }
                    ).expect("Failed to find 64 bit arch in FAT archive")?;
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
                    if name.starts_with('_') {
                        symbols.insert(name[1..].to_string(), value.n_value + offset);
                    }

                }
            }
            Ok(BinaryInfo{symbols, bss_addr, bss_size, offset})
        }

        Object::Elf(elf) => {
            let bss_header = elf.section_headers
                .iter()
                .find(|ref header| header.sh_type == goblin::elf::section_header::SHT_NOBITS)
                .expect("Failed to find BSS section header in ELF binary");

            let program_header = elf.program_headers
                .iter()
                .find(|ref header|
                    header.p_type == goblin::elf::program_header::PT_LOAD &&
                    header.p_flags & goblin::elf::program_header::PF_X != 0)
                .expect("Failed to find executable PT_LOAD program header in ELF binary");

            let offset = offset - program_header.p_vaddr;

            for sym in elf.syms.iter() {
                let name = elf.strtab[sym.st_name].to_string();
                symbols.insert(name, sym.st_value + offset);
            }
            Ok(BinaryInfo{symbols,
                          bss_addr: bss_header.sh_addr + offset,
                          bss_size: bss_header.sh_size,
                          offset})
        },
        Object::PE(pe) => {
            for export in pe.exports {
                let name = export.name;
                symbols.insert(name.to_string(), export.offset as u64 + offset as u64);
            }

            let data_section = pe.sections
                .iter()
                .find(|ref section| section.name.starts_with(b".data"))
                .expect("Failed to find .data section in PE binary");

            let bss_addr = u64::from(data_section.virtual_address) + offset;
            let bss_size = u64::from(data_section.virtual_size);

            Ok(BinaryInfo{symbols, bss_addr, bss_size, offset})
        },
        _ => {
            Err(GoblinError::Malformed(String::from("Unhandled binary type")))
        }
    }
}