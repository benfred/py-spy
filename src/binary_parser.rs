
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use goblin;
use goblin::Object;
use goblin::error::Error as GoblinError;

pub struct BinaryInfo {
    pub symbols: HashMap<String, u64>,
    pub bss_addr: u64,
    pub bss_size: u64,
    pub offset: u64
}

/// Uses goblin to parse a binary file, returns information on symbols/bss/adjusted offset etc
pub fn parse_binary(filename: &str, offset: u64) -> Result<BinaryInfo, GoblinError> {
    // Read in the filename
    let mut fd = File::open(Path::new(filename))?;
    let mut buffer = Vec::new();
    fd.read_to_end(&mut buffer)?;

    // Use goblin to parse the binary
    match Object::parse(&buffer)? {
        Object::Mach(mach) => {
            match mach {
                goblin::mach::Mach::Binary(macho) => {
                    parse_mach(macho, offset)
                },
                goblin::mach::Mach::Fat(fat) => {
                    // have a mach fat archive, get the symbols from the 64 bit binary
                    let arch = fat.iter_arches().find(|arch|
                        match arch {
                            Ok(arch) => arch.is_64(),
                            Err(_) => false
                        }
                    ).expect("Failed to find 64 bit arch in FAT archive")?;

                    let bytes = &buffer[arch.offset as usize..][..arch.size as usize];
                    parse_mach(goblin::mach::MachO::parse(bytes, 0)?, offset)
                }
            }
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

            let mut symbols = HashMap::new();
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
            let mut symbols = HashMap::new();
            for export in pe.exports {
                let name = export.name;
                symbols.insert(name.to_string(), export.offset as u64 + offset as u64);
            }

            let data_section = pe.sections
                .iter()
                .find(|ref section| section.name.starts_with(b".data"))
                .expect("Failed to find .data section in PE binary");

            let bss_addr = data_section.virtual_address as u64 + offset;
            let bss_size = data_section.virtual_size as u64;

            Ok(BinaryInfo{symbols, bss_addr, bss_size, offset})
        },
        _ => {
            Err(GoblinError::Malformed(String::from("Unhandled binary type")))
        }
    }
}

fn parse_mach(macho: goblin::mach::MachO, offset: u64) -> Result<BinaryInfo, GoblinError> {
    let mut bss_addr = 0;
    let mut bss_size = 0;
    for segment in macho.segments.iter() {
        for (section, _) in &segment.sections()? {
            if section.name()? == "__bss" {
                bss_addr = section.addr + offset;
                bss_size = section.size;
            }
        }
    }

    let mut symbols = HashMap::new();
    if let Some(syms) = macho.symbols {
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