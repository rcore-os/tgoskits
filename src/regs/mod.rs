mod accessors;
#[cfg(feature = "tracing")]
mod diff;
#[allow(unused_imports)]
pub use accessors::*;
#[cfg(feature = "tracing")]
pub use diff::*;

/// General-purpose registers for the 64-bit x86 architecture.
///
/// This structure holds the values of the general-purpose registers
/// used in 64-bit x86 systems, allowing for easy manipulation and storage of register states.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
// #[cfg_attr(feature = "tracing", derive(Snapshot))]
pub struct GeneralRegisters {
    /// The RAX register, typically used for return values in functions.
    pub rax: u64,
    /// The RCX register, often used as a counter in loops.
    pub rcx: u64,
    /// The RDX register, commonly used for I/O operations.
    pub rdx: u64,
    /// The RBX register, usually used as a base pointer or to store values across function calls.
    pub rbx: u64,
    /// Unused space for the RSP register, preserved for padding or future use.
    _unused_rsp: u64,
    /// The RBP register, often used as a frame pointer in function calls.
    pub rbp: u64,
    /// The RSI register, often used as a source index in string operations.
    pub rsi: u64,
    /// The RDI register, often used as a destination index in string operations.
    pub rdi: u64,
    /// The R8 register, an additional general-purpose register available in 64-bit mode.
    pub r8: u64,
    /// The R9 register, an additional general-purpose register available in 64-bit mode.
    pub r9: u64,
    /// The R10 register, an additional general-purpose register available in 64-bit mode.
    pub r10: u64,
    /// The R11 register, an additional general-purpose register available in 64-bit mode.
    pub r11: u64,
    /// The R12 register, an additional general-purpose register available in 64-bit mode.
    pub r12: u64,
    /// The R13 register, an additional general-purpose register available in 64-bit mode.
    pub r13: u64,
    /// The R14 register, an additional general-purpose register available in 64-bit mode.
    pub r14: u64,
    /// The R15 register, an additional general-purpose register available in 64-bit mode.
    pub r15: u64,
}

impl GeneralRegisters {
    /// The names of the general-purpose registers in 64-bit x86 architecture.
    ///
    /// We follow the order of registers opcode encoding.
    pub const REGISTER_NAMES: [&'static str; 16] = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ];

    /// Returns the name of the register corresponding to the given index.
    pub const fn register_name(index: u8) -> &'static str {
        Self::REGISTER_NAMES[index as usize]
    }
}

macro_rules! save_regs_to_stack {
    () => {
        "
        push r15
        push r14
        push r13
        push r12
        push r11
        push r10
        push r9
        push r8
        push rdi
        push rsi
        push rbp
        sub rsp, 8
        push rbx
        push rdx
        push rcx
        push rax"
    };
}

macro_rules! restore_regs_from_stack {
    () => {
        "
        pop rax
        pop rcx
        pop rdx
        pop rbx
        add rsp, 8
        pop rbp
        pop rsi
        pop rdi
        pop r8
        pop r9
        pop r10
        pop r11
        pop r12
        pop r13
        pop r14
        pop r15"
    };
}
