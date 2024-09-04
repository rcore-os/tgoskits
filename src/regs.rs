/// General-purpose registers for the 64-bit x86 architecture.
///
/// This structure holds the values of the general-purpose registers
/// used in 64-bit x86 systems, allowing for easy manipulation and storage of register states.
#[repr(C)]
#[derive(Debug, Default, Clone)]
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
    /// Returns the value of the general-purpose register corresponding to the given index.
    ///
    /// The mapping of indices to registers is as follows:
    /// - 0: `rax`
    /// - 1: `rcx`
    /// - 2: `rdx`
    /// - 3: `rbx`
    /// - 5: `rbp`
    /// - 6: `rsi`
    /// - 7: `rdi`
    /// - 8: `r8`
    /// - 9: `r9`
    /// - 10: `r10`
    /// - 11: `r11`
    /// - 12: `r12`
    /// - 13: `r13`
    /// - 14: `r14`
    /// - 15: `r15`
    ///
    /// # Panics
    ///
    /// This function will panic if the provided index is out of the range [0, 15] or if the index
    /// corresponds to an unused register (`rsp` at index 4).
    ///
    /// # Arguments
    ///
    /// * `index` - A `u8` value representing the index of the register.
    ///
    /// # Returns
    ///
    /// * `u64` - The value of the corresponding general-purpose register.
    pub fn get_reg_of_index(&self, index: u8) -> u64 {
        match index {
            0 => self.rax,
            1 => self.rcx,
            2 => self.rdx,
            3 => self.rbx,
            // 4 => self._unused_rsp,
            5 => self.rbp,
            6 => self.rsi,
            7 => self.rdi,
            8 => self.r8,
            9 => self.r9,
            10 => self.r10,
            11 => self.r11,
            12 => self.r12,
            13 => self.r13,
            14 => self.r14,
            15 => self.r15,
            _ => {
                panic!("Illegal index of GeneralRegisters {}", index);
            }
        }
    }

    /// Sets the value of the general-purpose register corresponding to the given index.
    ///
    /// The mapping of indices to registers is as follows:
    /// - 0: `rax`
    /// - 1: `rcx`
    /// - 2: `rdx`
    /// - 3: `rbx`
    /// - 5: `rbp`
    /// - 6: `rsi`
    /// - 7: `rdi`
    /// - 8: `r8`
    /// - 9: `r9`
    /// - 10: `r10`
    /// - 11: `r11`
    /// - 12: `r12`
    /// - 13: `r13`
    /// - 14: `r14`
    /// - 15: `r15`
    ///
    /// # Panics
    ///
    /// This function will panic if the provided index is out of the range [0, 15] or if the index
    /// corresponds to an unused register (`rsp` at index 4).
    ///
    /// # Arguments
    ///
    /// * `index` - A `u8` value representing the index of the register.
    ///
    /// # Returns
    ///
    /// * `u64` - The value of the corresponding general-purpose register.
    pub fn set_reg_of_index(&mut self, index: u8, value: u64) {
        match index {
            0 => self.rax = value,
            1 => self.rcx = value,
            2 => self.rdx = value,
            3 => self.rbx = value,
            // 4 => self._unused_rsp,
            5 => self.rbp = value,
            6 => self.rsi = value,
            7 => self.rdi = value,
            8 => self.r8 = value,
            9 => self.r9 = value,
            10 => self.r10 = value,
            11 => self.r11 = value,
            12 => self.r12 = value,
            13 => self.r13 = value,
            14 => self.r14 = value,
            15 => self.r15 = value,
            _ => {
                panic!("Illegal index of GeneralRegisters {}", index);
            }
        }
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
