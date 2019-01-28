use std;
use mach::structs::x86_thread_state64_t;
use mach_o_sys::compact_unwind_encoding::{unwind_info_section_header, unwind_info_section_header_index_entry,
    unwind_info_regular_second_level_page_header,
    unwind_info_compressed_second_level_page_header, unwind_info_regular_second_level_entry,
    UNWIND_SECOND_LEVEL_REGULAR, UNWIND_SECOND_LEVEL_COMPRESSED, UNWIND_SECTION_VERSION
};
use read_process_memory::{ProcessHandle};
use super::super::copy_struct;

// these are defined mach-o/compact_unwind_encoding.h (and in mach_o_sys crate), but
// I'm finding it easier to define here (defined as an enum in that crate, and I prefer const u32)
const UNWIND_X86_64_MODE_MASK: u32                      = 0xf000000;
const UNWIND_X86_64_MODE_RBP_FRAME: u32                 = 0x1000000;
const UNWIND_X86_64_MODE_STACK_IMMD: u32                = 0x2000000;
const UNWIND_X86_64_MODE_STACK_IND: u32                 = 0x3000000;
const UNWIND_X86_64_MODE_DWARF: u32                     = 0x4000000;
const UNWIND_X86_64_RBP_FRAME_REGISTERS: u32            = 0x7fff;
const UNWIND_X86_64_RBP_FRAME_OFFSET: u32               = 0xff0000;
const UNWIND_X86_64_FRAMELESS_STACK_SIZE:u32            = 0x00FF0000;
const UNWIND_X86_64_FRAMELESS_STACK_ADJUST:u32          = 0xe000;
const UNWIND_X86_64_FRAMELESS_STACK_REG_COUNT:u32       = 0x1c00;
const UNWIND_X86_64_FRAMELESS_STACK_REG_PERMUTATION:u32 = 0x3ff;
const UNWIND_X86_64_DWARF_SECTION_OFFSET:u32            = 0xffffff;

const UNWIND_X86_64_REG_NONE: u32 = 0;
const UNWIND_X86_64_REG_RBX: u32 = 1;
const UNWIND_X86_64_REG_R12: u32 = 2;
const UNWIND_X86_64_REG_R13: u32 = 3;
const UNWIND_X86_64_REG_R14: u32 = 4;
const UNWIND_X86_64_REG_R15: u32 = 5;
const UNWIND_X86_64_REG_RBP: u32 = 6;

pub struct CompactUnwindInfo {
    pub encoding: u32,
    pub func_start: u64,
    pub func_end: u64
}

#[derive(Debug)]
pub enum Error {
    UnknownMask(u32),
    DwarfUnwind,
    InvalidRegCount(u32),
    InvalidRegIndex(u32),
    InvalidHeaderVersion(u32),
    PageOutOfBounds,
    PCOutOfBounds,
    UnknownPageKind(u32),
    IOError(std::io::Error),
}

pub fn compact_unwind(info: &CompactUnwindInfo, reg: &mut x86_thread_state64_t, process: &ProcessHandle) -> Result<(), Error> {
    match info.encoding & UNWIND_X86_64_MODE_MASK {
        UNWIND_X86_64_MODE_RBP_FRAME => { compact_unwind_rbf(info, reg, process)?; },
        UNWIND_X86_64_MODE_STACK_IMMD  => { compact_unwind_stack(false, info, reg, process)?; },
        UNWIND_X86_64_MODE_STACK_IND  => { compact_unwind_stack(true, info, reg, process)?; },
        UNWIND_X86_64_MODE_DWARF => { return Err(Error::DwarfUnwind) },
        mask => { return Err(Error::UnknownMask(mask)) }
    };
    Ok(())
}

pub fn compact_unwind_rbf(info: &CompactUnwindInfo, reg: &mut x86_thread_state64_t, process: &ProcessHandle) -> Result<(), Error> {
    debug!("rbf unwind 0x{:016x}", reg.__rip);
    let registers_offset = 8 * extract_from_mask(info.encoding, UNWIND_X86_64_RBP_FRAME_OFFSET);
    let mut frame_registers = extract_from_mask(info.encoding, UNWIND_X86_64_RBP_FRAME_REGISTERS);
    // TODO: this next line can panic if registers_offset > reg.__rbp
    let saved_registers: [u64; 5] = copy_struct(reg.__rbp as usize - registers_offset as usize, process)?;
    for i in 0..5 {
        match frame_registers & 0x7 {
            UNWIND_X86_64_REG_NONE => {  },
            UNWIND_X86_64_REG_RBX => { reg.__rbx = saved_registers[i]; },
            UNWIND_X86_64_REG_R12 => { reg.__r12 = saved_registers[i]; },
            UNWIND_X86_64_REG_R13 => { reg.__r13 = saved_registers[i]; },
            UNWIND_X86_64_REG_R14 => { reg.__r14 = saved_registers[i]; },
            UNWIND_X86_64_REG_R15 => { reg.__r15 = saved_registers[i]; },
            _ => { /* TODO return an error? */ }
        }
        frame_registers = frame_registers >> 3;
    }
    // TODO: if this fails show a better error message. (Usually rbp register is pointing to invalid memory)
    let frame: [u64; 2] = copy_struct(reg.__rbp as usize, process)?;
    reg.__rsp = reg.__rbp + 16;
    reg.__rbp = frame[0];
    reg.__rip = frame[1];
    Ok(())
}

pub fn compact_unwind_stack(indirect_stack: bool,
                            info: &CompactUnwindInfo,
                            reg: &mut x86_thread_state64_t,
                            process: &ProcessHandle) -> Result<(), Error> {
    debug!("stacksize unwind 0x{:016x} (indirect={})", reg.__rip, indirect_stack);

    let reg_count = extract_from_mask(info.encoding, UNWIND_X86_64_FRAMELESS_STACK_REG_COUNT);
    let stack_adjust = extract_from_mask(info.encoding, UNWIND_X86_64_FRAMELESS_STACK_ADJUST);
    let mut reg_permutation = extract_from_mask(info.encoding, UNWIND_X86_64_FRAMELESS_STACK_REG_PERMUTATION);
    let mut stack_size = extract_from_mask(info.encoding, UNWIND_X86_64_FRAMELESS_STACK_SIZE);

    if indirect_stack {
        let offset: u32 = copy_struct(info.func_start as usize + stack_size as usize, process)?;
        stack_size = offset + 8 * stack_adjust;
    } else {
        stack_size *= 8;
    }
    debug!("reg_count {} reg_perm {} stack_size {} stack_adjust {}", reg_count, reg_permutation, stack_size, stack_adjust);

    // decode register permutations. algorithm to encode is given in mach-o/compact_unwind_encoding.h
    let reg_decoding = match reg_count {
        // with 6 registers, the last perm should always be 0
        // (meaning that both 5/6 reg_count have the same decode instructions since we 0 initialize)
        5 | 6 => &[120, 24, 6, 2, 1][..],
        4 => &[60, 12, 3, 1][..],
        3 => &[20, 4, 1][..],
        2 => &[5, 1][..],
        1 => &[1][..],
        0 => &[][..],
        _ => { return Err(Error::InvalidRegCount(reg_count)); }
    };

    let mut reg_perm = [0_u32; 6];
    for (i, v) in reg_decoding.iter().enumerate() {
        reg_perm[i] = reg_permutation / v;
        reg_permutation -= reg_perm[i] * v;
    }

    let mut used_regs = [false; 6];
    let mut reg_index = [0_u32; 6];
    for i in 0..(reg_count as usize) {
        let mut register_num = 0_u32 ;
        for j in 0..6 {
            if !used_regs[j] {
                if register_num == reg_perm[i] {
                    used_regs[j] = true;
                    reg_index[i] = (j + 1) as u32;
                    break;
                } else {
                    register_num += 1;
                }
            }
        }
    }
    debug!("reg index {:?}", reg_index);
    let save_offset =  reg.__rsp as usize + stack_size as usize - 8;

    debug!("save_offset {} stack_size {}", save_offset, stack_size);
    let saved_registers: [u64; 6] = copy_struct(save_offset - 8 * reg_count as usize, process)?;
    for i in 0..(reg_count as usize) {
        match reg_index[i] {
            UNWIND_X86_64_REG_NONE => {  },
            UNWIND_X86_64_REG_RBX => { reg.__rbx = saved_registers[i]; },
            UNWIND_X86_64_REG_R12 => { reg.__r12 = saved_registers[i]; },
            UNWIND_X86_64_REG_R13 => { reg.__r13 = saved_registers[i]; },
            UNWIND_X86_64_REG_R14 => { reg.__r14 = saved_registers[i]; },
            UNWIND_X86_64_REG_R15 => { reg.__r15 = saved_registers[i]; },
            UNWIND_X86_64_REG_RBP => { reg.__rbp = saved_registers[i]; },
            _ => { return Err(Error::InvalidRegIndex(reg_index[i])) }
        }
    }

    // TODO: my initial version had a bug (read rsp from memory instead of incrementing by stack size)
    // get a test that tests this (core dump of firefox?)
    reg.__rip = copy_struct(save_offset, process)?;
    reg.__rsp += stack_size as u64;
    Ok(())
}

pub fn get_compact_unwind_info(unwind_info: &[u8], mach_address: u64, pc: u64) -> Result<CompactUnwindInfo, Error> {
    let unwind_header = unwind_info as * const _ as * const unwind_info_section_header;

    // Get a slice of unwind_info_section_header_index_entry
    let index = unsafe {
        if (*unwind_header).version != UNWIND_SECTION_VERSION as u32 {
            return Err(Error::InvalidHeaderVersion((*unwind_header).version));
        }

        let index_buffer = &unwind_info[(*unwind_header).indexSectionOffset as usize..];
        std::slice::from_raw_parts(index_buffer.as_ptr() as *const unwind_info_section_header_index_entry,
                                    (*unwind_header).indexCount as usize)
    };

    // get the unwind index entry for the address
    let target_offset = (pc - mach_address as u64) as u32;
    let i = match index.binary_search_by(|index| index.functionOffset.cmp(&target_offset)) {
        Ok(v) => v,
        Err(v) => if v > 0 { v - 1 } else { v }
    };

    // The last element in the index isn't valid (shows end range). If we've hit that then we can't find the
    // unwind info for this address
    if i + 1 >= index.len() {
        return Err(Error::PageOutOfBounds);
    }

    let entry = &index[i];
    let next_entry = &index[i+1];

    // figure out the type of the second level index
    let second_level_buffer = &unwind_info[entry.secondLevelPagesSectionOffset as usize..];
    let page_kind = unsafe { *(second_level_buffer.as_ptr() as * const u32) };

    match page_kind as u8 {
        UNWIND_SECOND_LEVEL_REGULAR => {
            let second_level_header = second_level_buffer as * const _ as * const unwind_info_regular_second_level_page_header;
            let second_level_index = unsafe {
                let entry_buffer = &second_level_buffer[(*second_level_header).entryPageOffset as usize..];
                std::slice::from_raw_parts(entry_buffer.as_ptr() as *const unwind_info_regular_second_level_entry,
                    (*second_level_header).entryCount as usize)
            };

            let element = match second_level_index.binary_search_by(|e| e.functionOffset.cmp(&target_offset)) {
                Ok(v) => v,
                Err(v) => if v > 0 { v - 1 } else { v }
            };

            let second_level_entry = second_level_index[element];
            let func_start = second_level_entry.functionOffset as u64 + mach_address as u64;
            let func_end = if element + 1 < second_level_index.len() {
                (second_level_index[element + 1].functionOffset) as u64 + mach_address as u64
            } else {
                next_entry.functionOffset as u64 + mach_address as u64
            };

            if pc < func_start || pc >= func_end {
                return Err(Error::PCOutOfBounds);
            }

            return Ok(CompactUnwindInfo{encoding: second_level_entry.encoding, func_start, func_end: func_end});
        },
        UNWIND_SECOND_LEVEL_COMPRESSED => {
            // Get the page index
            let second_level_header = second_level_buffer as * const _ as * const unwind_info_compressed_second_level_page_header;
            let second_level_index = unsafe {
                let entry_buffer = &second_level_buffer[(*second_level_header).entryPageOffset as usize..];
                std::slice::from_raw_parts(entry_buffer.as_ptr() as *const u32,
                                            (*second_level_header).entryCount as usize)
            };

            let second_level_offset = target_offset - entry.functionOffset;
            let element = match second_level_index.binary_search_by(|e| (e & 0x00FFFFFF).cmp(&second_level_offset)) {
                Ok(v) => v,
                Err(v) => if v > 0 { v - 1 } else { v }
            };

            let second_level_entry = second_level_index[element];

            // Get the function start/end from the index
            let function_offset = mach_address + entry.functionOffset as u64;
            let func_start = (second_level_entry & 0x00FFFFFF) as u64 + function_offset;
            let func_end = if element + 1 < second_level_index.len() {
                (second_level_index[element + 1] & 0x00FFFFFF) as u64 + function_offset
            } else {
                next_entry.functionOffset as u64 + mach_address as u64
            };

            if pc < func_start || pc >= func_end {
                return Err(Error::PCOutOfBounds);
            }

            let encoding_index = (second_level_entry >> 24) & 0xFF;
            let encoding = unsafe {
                let common_encoding_count = (*unwind_header).commonEncodingsArrayCount;
                if encoding_index < common_encoding_count {
                    // encoding is stored in the unwind header array
                    let encodings_buffer = &unwind_info[(*unwind_header).commonEncodingsArraySectionOffset as usize..];
                    let encodings = std::slice::from_raw_parts(encodings_buffer.as_ptr() as *const u32,
                                                                common_encoding_count as usize);
                    encodings[encoding_index as usize]
                } else {
                    let encodings_buffer = &second_level_buffer[(*second_level_header).encodingsPageOffset as usize..];
                    let encodings = std::slice::from_raw_parts(encodings_buffer.as_ptr() as *const u32,
                                                                (*second_level_header).encodingsCount as usize);
                    encodings[(encoding_index - common_encoding_count) as usize]
                }
            };
            return Ok(CompactUnwindInfo{encoding, func_start, func_end});
        },
        _ => {
            return Err(Error::UnknownPageKind(page_kind));
        }
    }
}

/// Returns the offset into the eh_frame section if the compact encoding is for dwarf debugging entries
pub fn get_dwarf_offset(encoding: u32) -> Option<u32> {
    if  encoding & UNWIND_X86_64_MODE_MASK == UNWIND_X86_64_MODE_DWARF {
        Some(encoding & UNWIND_X86_64_DWARF_SECTION_OFFSET)
    } else {
        None
    }
}

// TODO: move into a utils module ?
fn extract_from_mask(value: u32, mask: u32) -> u32 {
    (value >> mask.trailing_zeros()) & (((1 << mask.count_ones()))-1)
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Error::UnknownMask(mask) => { write!(f, "Unknown compact encoding mask 0x{:x}", mask) },
            Error::DwarfUnwind => { write!(f, "encoding UNWIND_X86_64_MODE_DWARF can't be handle by compact_unwind") },
            Error::InvalidRegCount(count) => { write!(f, "invalid reg_count in frameless unwind {}", count) },
            Error::InvalidRegIndex(index) => { write!(f, "invalid reg_index in frameless unwind {}", index) },
            Error::InvalidHeaderVersion(ver) => { write!(f, "invalid unwind header version: {}", ver) },
            Error::PageOutOfBounds => { write!(f, "Compact page out of bounds") },
            Error::PCOutOfBounds => { write!(f, "PC isn't in bounds in compact unwind index") },
            Error::UnknownPageKind(page_kind) => { write!(f, "malformed unwind_info section: {}", page_kind) },
            Error::IOError(ref e) => e.fmt(f),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::IOError(err)
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str { "CompactUnwindError" }
    fn cause(&self) -> Option<&std::error::Error> {
        match *self {
            Error::IOError(ref e) => Some(e),
            _ => None,
        }
    }
}