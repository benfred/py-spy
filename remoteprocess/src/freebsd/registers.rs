/// Extract to dwarf_unwinder eventually

// sys/x86/include/reg.h
#[cfg(target_os="freebsd")]
#[derive(Debug, Default, Eq, PartialEq, Copy, Clone)]
pub struct Registers {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rax: u64,
    pub trapno: u32,
    pub fs: u16,
    pub gs: u16,
    pub err: u32,
    pub es: u16,
    pub ds: u16,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}


#[cfg(target_os="freebsd")]
fn get_register(regs: &Registers, register: gimli::Register) -> u64 {
    match register.0 {
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

#[cfg(target_os="freebsd")]
fn set_register(regs: &mut Registers, register: gimli::Register, value: u64) {
    match register.0 {
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
