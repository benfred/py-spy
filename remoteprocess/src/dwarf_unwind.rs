use gimli;
use read_process_memory::{ProcessHandle};

pub type RcReader = gimli::EndianRcSlice<gimli::NativeEndian>;
pub type FrameDescriptionEntry = gimli::FrameDescriptionEntry<gimli::EhFrame<RcReader>, RcReader>;
pub type UninitializedUnwindContext = gimli::UninitializedUnwindContext<gimli::EhFrame<RcReader>, RcReader>;
pub type InitializedUnwindContext = gimli::InitializedUnwindContext<gimli::EhFrame<RcReader>, RcReader>;

use super::copy_struct;

#[cfg(target_os="linux")]
use libc::c_ulonglong;

use gimli::UnwindSection;

#[cfg(target_os="macos")]
use std;

/// Contains dwarf debugging information for a single binary
#[derive(Debug)]
pub struct UnwindInfo {
    // eh_frame_hdr sections only exist on linux
    #[cfg(target_os="linux")]
    pub eh_frame_hdr: gimli::ParsedEhFrameHdr<RcReader>,

    // mach binaries don't contain an eh_frame_hdr section, so instead we
    // build a table of address:fde from the eh_frame section
    #[cfg(target_os="macos")]
    pub frame_descriptions: Vec<(u64, FrameDescriptionEntry)>,

    pub eh_frame: gimli::EhFrame<RcReader>,
    pub bases: gimli::BaseAddresses,
}

impl UnwindInfo {
    pub fn unwind(&self, reg: &mut Registers, process: &ProcessHandle) -> gimli::Result<bool> {
        // TODO: better registers abstraction
        #[cfg(target_os="macos")]
        let pc = reg.__rip - 1;

        #[cfg(target_os="linux")]
        let pc = reg.rip - 1;

        debug!("dwarf unwind 0x{:016x}", pc);

        let fde = match self.get_fde(pc) {
            Ok(fde) => fde,
            Err(e) => {
                #[cfg(target_os="macos")]
                let bp = reg.__rbp;

                #[cfg(target_os="linux")]
                let bp = reg.rbp;
                return match bp { 0 => Ok(false), _ => Err(e) };
            }
        };

        if !fde.contains(pc) {
            warn!("FDE doesn't contain pc 0x{:016x}", pc);
            #[cfg(target_os="linux")]
            return Err(gimli::Error::NoUnwindInfoForAddress);

            // TODO: on OSX figure out why this happens so much
            #[cfg(target_os="macos")]
            {
                debug!("FDE contains error. TODO: handle this {:?}", reg);
                return Ok(false);
            }
        }

        debug!("got fde covers range 0x{:016x}-0x{:016x}", fde.initial_address(), fde.initial_address() + fde.len());

        // TODO: reuse context?
        let ctx = UninitializedUnwindContext::new();
        let mut ctx = match ctx.initialize(fde.cie()) {
            Ok(ctx) => ctx,
            Err((e, _)) => return Err(e)
        };

        let row = get_unwind_row(pc, &mut ctx, &fde)?;
        let cfa = match *row.cfa() {
            gimli::CfaRule::RegisterAndOffset { register, offset } => {
                debug!("cfa rule register and offset: {}, {}", register, offset);
                get_register(reg, register).wrapping_add(offset as u64)
            },
            gimli::CfaRule::Expression(ref e) => {
                evaluate_dwarf_expression(e, None, reg, process)?
            }
        };

        debug!("cfa is 0x{:016x}", cfa);
        for &(register, ref rule) in row.registers() {
            let value = match *rule {
                gimli::RegisterRule::Offset(offset) => copy_struct(cfa.wrapping_add(offset as u64) as usize, process)?,
                gimli::RegisterRule::Register(r) => get_register(reg, r),
                gimli::RegisterRule::SameValue => get_register(reg, register),
                gimli::RegisterRule::ValOffset(offset) => cfa.wrapping_add(offset as u64),
                gimli::RegisterRule::Expression(ref e) => {
                    copy_struct(evaluate_dwarf_expression(e, Some(cfa), reg, process)? as usize, process)?
                },
                gimli::RegisterRule::ValExpression(ref e) => {
                    evaluate_dwarf_expression(e, Some(cfa), reg, process)?
                },
                gimli::RegisterRule::Architectural => unimplemented!("Unhandled dwarf rule: Architectural"),
                gimli::RegisterRule::Undefined => unimplemented!("Unhandled dwarf rule: Undefined"),
            };
            set_register(reg, register, value);
        }

        #[cfg(target_os="macos")]
        { reg.__rsp = cfa; }

        #[cfg(target_os="linux")]
        { reg.rsp = cfa; }

        Ok(true)
    }

    #[cfg(target_os="macos")]
    fn get_fde(&self, pc: u64) -> gimli::Result<&FrameDescriptionEntry> {
        // Binary search frame description table to get the FDE on osx
        if self.frame_descriptions.len() == 0 {
            return Err(gimli::Error::NoUnwindInfoForAddress);
        }
        let fde = match self.frame_descriptions.binary_search_by(|e| e.0.cmp(&pc)) {
            Ok(i) => &self.frame_descriptions[i].1,
            Err(v) => &self.frame_descriptions[if v > 0 { v - 1 } else { v }].1
        };
        Ok(fde)
    }

    #[cfg(target_os="linux")]
    fn get_fde(&self, pc: u64) -> gimli::Result<FrameDescriptionEntry> {
        // lookup FDE inside the eh_frame_hdr section on linux
        self.eh_frame_hdr.table().unwrap()
            .lookup_and_parse(pc, &self.bases,
                              self.eh_frame.clone(),  // can we do this without cloning?
                              |offset| self.eh_frame.cie_from_offset(&self.bases, offset))
    }

    /// Creates a new UnwindInfo object for OSX. This iterates over the eh_frame section
    /// and builds a lookup table of all the frame description entries found in there.
    /// This is expensive to compute, but is a one time cost (after which looking up the
    /// FDE is pretty cheap). Note: on linux we use the eh_frame_hdr section instead
    #[cfg(target_os="macos")]
    pub fn new(eh_frame: &[u8], eh_frame_address: u64) -> gimli::Result<UnwindInfo> {
        let buf = std::rc::Rc::from(eh_frame);
        let eh_frame = gimli::EhFrame::from(RcReader::new(buf, gimli::NativeEndian));
        let bases = gimli::BaseAddresses::default().set_cfi(eh_frame_address);

        // Get a vector of all the frame description entries
        let frame_descriptions = {
            let mut frame_descriptions = Vec::new();
            let mut iter = eh_frame.entries(&bases);
            while let Some(entry) = iter.next()? {
                match entry {
                    gimli::CieOrFde::Cie(_) => continue,
                    gimli::CieOrFde::Fde(partial) => {
                        let fde = partial.parse(|offset| eh_frame.cie_from_offset(&bases, offset))?;
                        frame_descriptions.push((fde.initial_address(), fde));
                    }
                }
            }
            frame_descriptions.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            frame_descriptions
        };

        Ok(UnwindInfo{eh_frame, bases, frame_descriptions})
    }
}

fn evaluate_dwarf_expression(e: &gimli::Expression<RcReader>, initial: Option<u64>, registers: &Registers, process: &ProcessHandle) -> gimli::Result<u64> {
    // TODO: this will require different code for 32bit
    let mut eval = e.clone().evaluation(8, gimli::Format::Dwarf64);

    if let Some(initial) = initial {
        eval.set_initial_value(initial);
    }

    let mut result = eval.evaluate()?;

    while result != gimli::EvaluationResult::Complete {
        match result {
            gimli::EvaluationResult::RequiresRegister{register, base_type} => {
                debug!("reguires register {:?} {:?}", register, base_type);
                result = eval.resume_with_register(gimli::Value::Generic(get_register(registers, register as _)))?;
            },
            gimli::EvaluationResult::RequiresMemory{address, size, space, base_type} => {
                debug!("reguires memory addr=0x{:016x} size={} space={:?} base_type={:?}", address, size, space, base_type);
                match size {
                    8 => {
                        let value: u64 = copy_struct(address as usize, process)?;
                        result = eval.resume_with_memory(gimli::Value::Generic(value))?;
                    },
                    4 => {
                        let value: u32 = copy_struct(address as usize, process)?;
                        result = eval.resume_with_memory(gimli::Value::Generic(value as u64))?;
                    }
                    _ => {
                        // TODO: this probably will never happen
                        error!("Unhandled dwarf expression. Size {} isn't handled for RequiresMemory", size);
                        return Err(gimli::Error::Io);
                    }
                }

            },
            other => { warn!("unhandled dwarf expression requirement{:?}", other); return Err(gimli::Error::Io); }
        };
    }

    let results = eval.result();

    if results.len() != 1 {
        warn!("Failed to evaluate_dwarf_expression, expected a single result - found {}", results.len());
        return Err(gimli::Error::Io);
    }

    match &results[0] {
        gimli::Piece{location: gimli::Location::Address{address}, ..} => Ok(address.clone()),
        other => {
            warn!("Unhandled dwarf evaluation result {:#?}", other);
            Err(gimli::Error::Io)
        }
    }
}

fn get_unwind_row(pc: u64, ctx: &mut InitializedUnwindContext, fde: &FrameDescriptionEntry)
            -> gimli::Result<gimli::UnwindTableRow<RcReader>> {
    let mut table = gimli::UnwindTable::new(ctx, &fde);
    while let Some(row) = table.next_row()? {
        if row.contains(pc) {
            return Ok(row.clone());
        }
    }
    error!("Failed to find unwind row for 0x{:016x}", pc);
    Err(gimli::Error::NoUnwindInfoForAddress)
}

// TODO: refactor register handling code (make functions methods, add getrsp/get_ip etc)
#[cfg(target_os="macos")]
use mach::structs::x86_thread_state64_t;

#[cfg(target_os="macos")]
pub type Registers = x86_thread_state64_t;

// This is identical to libc::user_regs_struct,
// which seems to be be missing for the musl toolchain we're using
// TODO: file a PR?
#[cfg(target_os="linux")]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct Registers {
    pub r15: c_ulonglong,
    pub r14: c_ulonglong,
    pub r13: c_ulonglong,
    pub r12: c_ulonglong,
    pub rbp: c_ulonglong,
    pub rbx: c_ulonglong,
    pub r11: c_ulonglong,
    pub r10: c_ulonglong,
    pub r9: c_ulonglong,
    pub r8: c_ulonglong,
    pub rax: c_ulonglong,
    pub rcx: c_ulonglong,
    pub rdx: c_ulonglong,
    pub rsi: c_ulonglong,
    pub rdi: c_ulonglong,
    pub orig_rax: c_ulonglong,
    pub rip: c_ulonglong,
    pub cs: c_ulonglong,
    pub eflags: c_ulonglong,
    pub rsp: c_ulonglong,
    pub ss: c_ulonglong,
    pub fs_base: c_ulonglong,
    pub gs_base: c_ulonglong,
    pub ds: c_ulonglong,
    pub es: c_ulonglong,
    pub fs: c_ulonglong,
    pub gs: c_ulonglong,
}

#[cfg(target_os="macos")]
fn get_register(regs: &x86_thread_state64_t, register: u8) -> u64 {
    unsafe {
        let regs = regs as *const _ as *const u64;
        *regs.offset(register as isize)
    }
}
#[cfg(target_os="macos")]
fn set_register(regs: &mut x86_thread_state64_t, register: u8, value: u64) {
    unsafe {
        let regs = regs as *mut _ as *mut u64;
        *regs.offset(register as isize) = value
    }
}

#[cfg(target_os="linux")]
fn get_register(regs: &Registers, register: u8) -> u64 {
    // ffs
    match register {
        0 => regs.rax,
        1 => regs.rdx,
        2 => regs.rcx,
        3 => regs.rbx,
        4 => regs.rsi,
        5 => regs.rdi,
        6 => regs.rbp,
        7 => regs.rsp,
        8 => regs.r8,
        9 => regs.r9,
        10 => regs.r10,
        11 => regs.r11,
        12 => regs.r12,
        13 => regs.r13,
        14 => regs.r14,
        15 => regs.r15,
        16 => regs.rip,
        _ => panic!("unknown reg")
    }
}

#[cfg(target_os="linux")]
fn set_register(regs: &mut Registers, register: u8, value: u64) {
    match register {
        0 => regs.rax = value,
        1 => regs.rdx = value,
        2 => regs.rcx = value,
        3 => regs.rbx = value,
        4 => regs.rsi = value,
        5 => regs.rdi = value,
        6 => regs.rbp = value ,
        7 => regs.rsp = value,
        8 => regs.r8 = value,
        9 => regs.r9 = value,
        10 => regs.r10 = value,
        11 => regs.r11 = value,
        12 => regs.r12 = value,
        13 => regs.r13 = value,
        14 => regs.r14 = value,
        15 => regs.r15 = value,
        16 => regs.rip = value,
        _ => panic!("unknown reg")
    }
}
