// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Architecture register images.

#[cfg(kernel_tls)]
pub use super::asm::{read_thread_pointer, write_thread_pointer};
pub use super::{
    context::{ExtendedState, FxsaveArea, UserXstate},
    entry_state::CpuEntryState,
    msr::Msr,
};

/// Installs the kernel GS base without requiring FSGSBASE support.
///
/// # Safety
/// The caller must own the offline or IRQ-disabled CPU binding transition.
/// `base` must be canonical and all memory used by GS-relative instructions
/// must remain mapped and valid until the binding is retired. SWAPGS entry
/// must agree on which bank currently contains the kernel base.
#[inline]
pub unsafe fn write_gs_base(base: usize) {
    // SAFETY: the caller supplies a valid kernel GS binding at its transition.
    unsafe { x86::msr::wrmsr(x86::msr::IA32_GS_BASE, base as u64) };
}

/// Reads a 32-bit word at a constant offset from the current GS base.
///
/// # Safety
/// GS plus `OFFSET` must address an initialized, readable, aligned word.
/// Its owner must permit this access and exclude incompatible remote writes.
/// The caller must retain the CPU binding for as long as the result needs it.
#[inline(always)]
pub unsafe fn read_gs_u32<const OFFSET: usize>() -> u32 {
    let value;
    // SAFETY: the caller supplies the live GS mapping and access ownership.
    unsafe {
        core::arch::asm!(
            "mov {value:e}, dword ptr gs:[{offset}]",
            value = out(reg) value,
            offset = const OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    value
}

/// Reads a machine word at a constant offset from the current GS base.
///
/// # Safety
/// GS plus `OFFSET` must address an initialized, readable, aligned word.
/// Its owner must permit this access and exclude incompatible remote writes.
/// The caller must retain the CPU binding for as long as the result needs it.
#[inline(always)]
pub unsafe fn read_gs_usize<const OFFSET: usize>() -> usize {
    let value;
    // SAFETY: the caller supplies the live GS mapping and access ownership.
    unsafe {
        core::arch::asm!(
            "mov {value}, qword ptr gs:[{offset}]",
            value = out(reg) value,
            offset = const OFFSET,
            options(nostack, preserves_flags, readonly),
        );
    }
    value
}

/// Increments a CPU-owned GS-relative word, wrapping at 32 bits.
///
/// This is one instruction with respect to local interrupt delivery. It has
/// no LOCK prefix and provides no inter-CPU synchronization.
///
/// # Safety
/// GS plus `OFFSET` must identify a live, aligned, writable word exclusively
/// owned by this CPU. The memory owner must permit modification by this
/// instruction and no remote access may race it.
#[inline(always)]
pub unsafe fn increment_gs_u32<const OFFSET: usize>() {
    // SAFETY: the caller owns the selected writable word on this CPU.
    unsafe {
        core::arch::asm!(
            "inc dword ptr gs:[{offset}]",
            offset = const OFFSET,
            options(nostack),
        );
    }
}

/// Decrements a CPU-owned GS-relative word, wrapping at 32 bits.
///
/// This is one instruction with respect to local interrupt delivery. It has
/// no LOCK prefix and provides no inter-CPU synchronization.
///
/// # Safety
/// GS plus `OFFSET` must identify a live, aligned, writable word exclusively
/// owned by this CPU. The memory owner must permit modification by this
/// instruction and no remote access may race it.
#[inline(always)]
pub unsafe fn decrement_gs_u32<const OFFSET: usize>() {
    // SAFETY: the caller owns the selected writable word on this CPU.
    unsafe {
        core::arch::asm!(
            "dec dword ptr gs:[{offset}]",
            offset = const OFFSET,
            options(nostack),
        );
    }
}

/// Compares and conditionally replaces a CPU-owned GS-relative word.
///
/// Returns the observed value. This instruction is atomic only with respect
/// to local interrupt delivery; it has no LOCK prefix or inter-CPU ordering.
///
/// # Safety
/// GS plus `OFFSET` must identify a live, aligned, writable word exclusively
/// owned by this CPU. The memory owner must permit modification by this
/// instruction and no remote access may race it, even on comparison failure.
#[inline(always)]
pub unsafe fn compare_exchange_gs_u32<const OFFSET: usize>(current: u32, next: u32) -> u32 {
    let observed;
    // SAFETY: the caller owns the selected writable word on this CPU.
    unsafe {
        core::arch::asm!(
            "cmpxchg dword ptr gs:[{offset}], {next:e}",
            offset = const OFFSET,
            next = in(reg) next,
            inout("eax") current => observed,
            options(nostack),
        );
    }
    observed
}

/// General-purpose registers for the 64-bit x86 architecture.
///
/// This structure holds the values of the general-purpose registers
/// saved by software in trap and guest-entry frames. RSP belongs to the
/// hardware return frame or virtualization control image and is stored separately.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GeneralRegisters {
    /// The RAX register, typically used for return values in functions.
    pub rax: u64,
    /// The RCX register, often used as a counter in loops.
    pub rcx: u64,
    /// The RDX register, commonly used for I/O operations.
    pub rdx: u64,
    /// The RBX register, usually used as a base pointer or to store values across function calls.
    pub rbx: u64,
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

    /// Returns the value of the `edx:eax` register pair.
    pub fn get_edx_eax(&self) -> u64 {
        (self.edx() as u64) << 32 | self.eax() as u64
    }
}

macro_rules! register_accessors {
    (
        $get:ident,
        $set:ident,
        $field:ident,
        $ty:ty,
        $shift:literal,
        $mask:expr,
        $zero_extend:literal
    ) => {
        #[doc = concat!("Reads the `", stringify!($get), "` subregister.")]
        pub const fn $get(&self) -> $ty {
            (self.$field >> $shift) as $ty
        }
        #[doc = concat!("Writes `", stringify!($get), "` using x86 subregister semantics.")]
        pub const fn $set(&mut self, value: $ty) {
            if $zero_extend {
                self.$field = value as u64;
            } else {
                self.$field =
                    (self.$field & !(($mask as u64) << $shift)) | ((value as u64) << $shift);
            }
        }
    };
}

impl GeneralRegisters {
    register_accessors!(eax, set_eax, rax, u32, 0, u32::MAX, true);
    register_accessors!(ecx, set_ecx, rcx, u32, 0, u32::MAX, true);
    register_accessors!(edx, set_edx, rdx, u32, 0, u32::MAX, true);
    register_accessors!(ebx, set_ebx, rbx, u32, 0, u32::MAX, true);
    register_accessors!(ebp, set_ebp, rbp, u32, 0, u32::MAX, true);
    register_accessors!(esi, set_esi, rsi, u32, 0, u32::MAX, true);
    register_accessors!(edi, set_edi, rdi, u32, 0, u32::MAX, true);
    register_accessors!(r8d, set_r8d, r8, u32, 0, u32::MAX, true);
    register_accessors!(r9d, set_r9d, r9, u32, 0, u32::MAX, true);
    register_accessors!(r10d, set_r10d, r10, u32, 0, u32::MAX, true);
    register_accessors!(r11d, set_r11d, r11, u32, 0, u32::MAX, true);
    register_accessors!(r12d, set_r12d, r12, u32, 0, u32::MAX, true);
    register_accessors!(r13d, set_r13d, r13, u32, 0, u32::MAX, true);
    register_accessors!(r14d, set_r14d, r14, u32, 0, u32::MAX, true);
    register_accessors!(r15d, set_r15d, r15, u32, 0, u32::MAX, true);
    register_accessors!(ax, set_ax, rax, u16, 0, u16::MAX, false);
    register_accessors!(cx, set_cx, rcx, u16, 0, u16::MAX, false);
    register_accessors!(dx, set_dx, rdx, u16, 0, u16::MAX, false);
    register_accessors!(bx, set_bx, rbx, u16, 0, u16::MAX, false);
    register_accessors!(bp, set_bp, rbp, u16, 0, u16::MAX, false);
    register_accessors!(si, set_si, rsi, u16, 0, u16::MAX, false);
    register_accessors!(di, set_di, rdi, u16, 0, u16::MAX, false);
    register_accessors!(r8w, set_r8w, r8, u16, 0, u16::MAX, false);
    register_accessors!(r9w, set_r9w, r9, u16, 0, u16::MAX, false);
    register_accessors!(r10w, set_r10w, r10, u16, 0, u16::MAX, false);
    register_accessors!(r11w, set_r11w, r11, u16, 0, u16::MAX, false);
    register_accessors!(r12w, set_r12w, r12, u16, 0, u16::MAX, false);
    register_accessors!(r13w, set_r13w, r13, u16, 0, u16::MAX, false);
    register_accessors!(r14w, set_r14w, r14, u16, 0, u16::MAX, false);
    register_accessors!(r15w, set_r15w, r15, u16, 0, u16::MAX, false);
    register_accessors!(al, set_al, rax, u8, 0, u8::MAX, false);
    register_accessors!(cl, set_cl, rcx, u8, 0, u8::MAX, false);
    register_accessors!(dl, set_dl, rdx, u8, 0, u8::MAX, false);
    register_accessors!(bl, set_bl, rbx, u8, 0, u8::MAX, false);
    register_accessors!(bpl, set_bpl, rbp, u8, 0, u8::MAX, false);
    register_accessors!(sil, set_sil, rsi, u8, 0, u8::MAX, false);
    register_accessors!(dil, set_dil, rdi, u8, 0, u8::MAX, false);
    register_accessors!(r8b, set_r8b, r8, u8, 0, u8::MAX, false);
    register_accessors!(r9b, set_r9b, r9, u8, 0, u8::MAX, false);
    register_accessors!(r10b, set_r10b, r10, u8, 0, u8::MAX, false);
    register_accessors!(r11b, set_r11b, r11, u8, 0, u8::MAX, false);
    register_accessors!(r12b, set_r12b, r12, u8, 0, u8::MAX, false);
    register_accessors!(r13b, set_r13b, r13, u8, 0, u8::MAX, false);
    register_accessors!(r14b, set_r14b, r14, u8, 0, u8::MAX, false);
    register_accessors!(r15b, set_r15b, r15, u8, 0, u8::MAX, false);
    register_accessors!(ah, set_ah, rax, u8, 8, u8::MAX, false);
    register_accessors!(ch, set_ch, rcx, u8, 8, u8::MAX, false);
    register_accessors!(dh, set_dh, rdx, u8, 8, u8::MAX, false);
    register_accessors!(bh, set_bh, rbx, u8, 8, u8::MAX, false);
}
