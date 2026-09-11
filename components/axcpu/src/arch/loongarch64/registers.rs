//! Architecture register images.

#[cfg(kernel_tls)]
pub use super::asm::{read_thread_pointer, write_thread_pointer};
pub use super::{
    asm::{enable_fp, enable_lasx, enable_lsx},
    fp::FpuState,
};

/// Kernel stack pointer scratch slot used by exception entry.
pub const KSAVE_KSP: usize = 0;
/// First temporary-register scratch slot used by exception entry.
pub const KSAVE_T0: usize = 1;
/// Second temporary-register scratch slot used by exception entry.
pub const KSAVE_T1: usize = 2;
/// CPU-local runtime area-base shadow restored by exception entry.
pub const KSAVE_PERCPU: usize = 3;

/// Host CPU-local runtime area-base shadow.
pub const HOST_PERCPU_KS: usize = KSAVE_PERCPU;
/// Host stack scratch reserved for vCPU entry and exit.
pub const HOST_VCPU_KS: usize = 4;
/// Temporary scratch reserved for vCPU entry and exit.
pub const HOST_VCPU_TMP_KS: usize = 5;

/// Reads the raw thread-pointer register without interpreting its contents.
#[inline]
pub fn read_tp() -> usize {
    let value;
    // SAFETY: this copies a register and does not dereference its value.
    unsafe { core::arch::asm!("move {}, $tp", out(reg) value, options(nostack)) };
    value
}

/// Installs the raw thread-pointer register.
///
/// # Safety
/// The caller must own the final TLS or task-anchor transition and retain the
/// selected object. Subsequent Rust code and exception entry must agree on
/// the new binding before they can use it.
#[inline]
pub unsafe fn write_tp(value: usize) {
    // SAFETY: the caller owns the transition and the selected object.
    unsafe { core::arch::asm!("move $tp, {}", in(reg) value, options(nostack)) };
}

/// Reads the reserved CPU anchor register and its exception-entry shadow.
///
/// The runtime validates that the two values agree before following either.
#[inline]
pub fn read_cpu_anchor() -> (usize, usize) {
    let base;
    let shadow;
    // SAFETY: this copies the CPU registers without following either value.
    unsafe {
        core::arch::asm!(
            "move {base}, $r21",
            "csrrd {shadow}, {scratch}",
            base = out(reg) base,
            shadow = out(reg) shadow,
            scratch = const 0x30 + KSAVE_PERCPU,
            options(nostack),
        );
    }
    (base, shadow)
}

/// Installs the reserved CPU anchor register and its exception-entry shadow.
///
/// # Safety
/// The caller must own offline CPU initialization with traps disabled. The
/// selected memory must remain alive and mapped until the binding is retired;
/// exception entry must use the same scratch-slot convention.
#[inline]
pub unsafe fn write_cpu_anchor(value: usize) {
    // SAFETY: traps are excluded across both writes by the caller's contract.
    unsafe {
        core::arch::asm!(
            "csrwr {shadow}, {scratch}",
            "move $r21, {base}",
            shadow = inout(reg) value => _,
            base = in(reg) value,
            scratch = const 0x30 + KSAVE_PERCPU,
            options(nostack),
        );
    }
}

/// Reads one host control register.
///
/// # Safety
/// The caller must execute at the required privilege level, select an implemented
/// register, and own any register-read side effects on this CPU.
#[inline]
pub unsafe fn read_csr<const CSR: u16>() -> usize {
    let value;
    // SAFETY: the caller owns the selected implemented CSR and its side effects.
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) value, const CSR, options(nostack));
    }
    value
}

/// Reads one guest control register.
///
/// # Safety
/// The caller must execute at the required privilege level, select an implemented
/// register, and own any register-read side effects on this CPU.
#[inline]
pub unsafe fn read_guest_csr<const CSR: usize>() -> usize {
    let value;
    // SAFETY: the caller owns the selected implemented CSR and its side effects.
    unsafe {
        core::arch::asm!("gcsrrd {}, {}", out(reg) value, const CSR, options(nostack));
    }
    value
}

/// Writes one host control register.
///
/// # Safety
/// The caller must exclusively own the selected implemented register, satisfy
/// its reserved-bit and lifecycle requirements, and exclude conflicting IRQ access.
#[inline]
pub unsafe fn write_csr<const CSR: u16>(value: usize) {
    // SAFETY: the caller owns the local CSR transition and validates its value.
    unsafe {
        core::arch::asm!("csrwr {}, {}", inout(reg) value => _, const CSR, options(nostack));
    }
}

/// Writes one guest control register.
///
/// # Safety
/// The caller must exclusively own the selected implemented register, satisfy
/// its reserved-bit and lifecycle requirements, and exclude conflicting IRQ access.
#[inline]
pub unsafe fn write_guest_csr<const CSR: usize>(value: usize) {
    // SAFETY: the caller owns the local CSR transition and validates its value.
    unsafe {
        core::arch::asm!("gcsrwr {}, {}", inout(reg) value => _, const CSR, options(nostack));
    }
}

/// Reads a 8-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width and
/// device lifetime, and own any read side effects. CPU code does not own devices.
#[inline]
pub unsafe fn read_iocsr8(address: usize) -> u8 {
    let value: usize;
    // SAFETY: the platform owner validated this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrrd.b {}, {}", out(reg) value, in(reg) address, options(nostack));
    }
    value as u8
}

/// Writes a 8-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width, value
/// and device lifetime, and serialize accesses according to the device protocol.
#[inline]
pub unsafe fn write_iocsr8(address: usize, value: u8) {
    // SAFETY: the platform owner validated and serialized this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrwr.b {}, {}", in(reg) value as usize, in(reg) address, options(nostack));
    }
}

/// Reads a 16-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width and
/// device lifetime, and own any read side effects. CPU code does not own devices.
#[inline]
pub unsafe fn read_iocsr16(address: usize) -> u16 {
    let value: usize;
    // SAFETY: the platform owner validated this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrrd.h {}, {}", out(reg) value, in(reg) address, options(nostack));
    }
    value as u16
}

/// Writes a 16-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width, value
/// and device lifetime, and serialize accesses according to the device protocol.
#[inline]
pub unsafe fn write_iocsr16(address: usize, value: u16) {
    // SAFETY: the platform owner validated and serialized this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrwr.h {}, {}", in(reg) value as usize, in(reg) address, options(nostack));
    }
}

/// Reads a 32-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width and
/// device lifetime, and own any read side effects. CPU code does not own devices.
#[inline]
pub unsafe fn read_iocsr32(address: usize) -> u32 {
    let value: usize;
    // SAFETY: the platform owner validated this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrrd.w {}, {}", out(reg) value, in(reg) address, options(nostack));
    }
    value as u32
}

/// Writes a 32-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width, value
/// and device lifetime, and serialize accesses according to the device protocol.
#[inline]
pub unsafe fn write_iocsr32(address: usize, value: u32) {
    // SAFETY: the platform owner validated and serialized this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrwr.w {}, {}", in(reg) value as usize, in(reg) address, options(nostack));
    }
}

/// Reads a 64-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width and
/// device lifetime, and own any read side effects. CPU code does not own devices.
#[inline]
pub unsafe fn read_iocsr64(address: usize) -> u64 {
    let value: usize;
    // SAFETY: the platform owner validated this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) address, options(nostack));
    }
    value as u64
}

/// Writes a 64-bit IOCSR register.
///
/// # Safety
/// The platform owner must validate the register address, access width, value
/// and device lifetime, and serialize accesses according to the device protocol.
#[inline]
pub unsafe fn write_iocsr64(address: usize, value: u64) {
    // SAFETY: the platform owner validated and serialized this IOCSR operation.
    unsafe {
        core::arch::asm!("iocsrwr.d {}, {}", in(reg) value as usize, in(reg) address, options(nostack));
    }
}

/// General registers of Loongarch64.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    /// Architectural `zero` register image.
    pub zero: usize,
    /// Architectural `ra` register image.
    pub ra: usize,
    /// Architectural `tp` register image.
    pub tp: usize,
    /// Architectural `sp` register image.
    pub sp: usize,
    /// Architectural `a0` register image.
    pub a0: usize,
    /// Architectural `a1` register image.
    pub a1: usize,
    /// Architectural `a2` register image.
    pub a2: usize,
    /// Architectural `a3` register image.
    pub a3: usize,
    /// Architectural `a4` register image.
    pub a4: usize,
    /// Architectural `a5` register image.
    pub a5: usize,
    /// Architectural `a6` register image.
    pub a6: usize,
    /// Architectural `a7` register image.
    pub a7: usize,
    /// Architectural `t0` register image.
    pub t0: usize,
    /// Architectural `t1` register image.
    pub t1: usize,
    /// Architectural `t2` register image.
    pub t2: usize,
    /// Architectural `t3` register image.
    pub t3: usize,
    /// Architectural `t4` register image.
    pub t4: usize,
    /// Architectural `t5` register image.
    pub t5: usize,
    /// Architectural `t6` register image.
    pub t6: usize,
    /// Architectural `t7` register image.
    pub t7: usize,
    /// Architectural `t8` register image.
    pub t8: usize,
    /// User `u0` on a user-origin trap; a diagnostic per-CPU snapshot on a
    /// kernel-origin trap. Kernel return deliberately ignores this field.
    pub u0: usize,
    /// Architectural `fp` register image.
    pub fp: usize,
    /// Architectural `s0` register image.
    pub s0: usize,
    /// Architectural `s1` register image.
    pub s1: usize,
    /// Architectural `s2` register image.
    pub s2: usize,
    /// Architectural `s3` register image.
    pub s3: usize,
    /// Architectural `s4` register image.
    pub s4: usize,
    /// Architectural `s5` register image.
    pub s5: usize,
    /// Architectural `s6` register image.
    pub s6: usize,
    /// Architectural `s7` register image.
    pub s7: usize,
    /// Architectural `s8` register image.
    pub s8: usize,
}

impl core::ops::Index<usize> for GeneralRegisters {
    type Output = usize;
    fn index(&self, index: usize) -> &usize {
        match index {
            0 => &self.zero,
            1 => &self.ra,
            2 => &self.tp,
            3 => &self.sp,
            4 => &self.a0,
            5 => &self.a1,
            6 => &self.a2,
            7 => &self.a3,
            8 => &self.a4,
            9 => &self.a5,
            10 => &self.a6,
            11 => &self.a7,
            12 => &self.t0,
            13 => &self.t1,
            14 => &self.t2,
            15 => &self.t3,
            16 => &self.t4,
            17 => &self.t5,
            18 => &self.t6,
            19 => &self.t7,
            20 => &self.t8,
            21 => &self.u0,
            22 => &self.fp,
            23 => &self.s0,
            24 => &self.s1,
            25 => &self.s2,
            26 => &self.s3,
            27 => &self.s4,
            28 => &self.s5,
            29 => &self.s6,
            30 => &self.s7,
            31 => &self.s8,
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }
}

impl core::ops::IndexMut<usize> for GeneralRegisters {
    fn index_mut(&mut self, index: usize) -> &mut usize {
        match index {
            0 => &mut self.zero,
            1 => &mut self.ra,
            2 => &mut self.tp,
            3 => &mut self.sp,
            4 => &mut self.a0,
            5 => &mut self.a1,
            6 => &mut self.a2,
            7 => &mut self.a3,
            8 => &mut self.a4,
            9 => &mut self.a5,
            10 => &mut self.a6,
            11 => &mut self.a7,
            12 => &mut self.t0,
            13 => &mut self.t1,
            14 => &mut self.t2,
            15 => &mut self.t3,
            16 => &mut self.t4,
            17 => &mut self.t5,
            18 => &mut self.t6,
            19 => &mut self.t7,
            20 => &mut self.t8,
            21 => &mut self.u0,
            22 => &mut self.fp,
            23 => &mut self.s0,
            24 => &mut self.s1,
            25 => &mut self.s2,
            26 => &mut self.s3,
            27 => &mut self.s4,
            28 => &mut self.s5,
            29 => &mut self.s6,
            30 => &mut self.s7,
            31 => &mut self.s8,
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }
}
