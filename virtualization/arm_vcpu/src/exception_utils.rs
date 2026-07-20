use aarch64_cpu::registers::*;

use crate::{ArmDataAbortSyndrome, ArmFaultIpa, ArmVcpuError, ArmVcpuResult};

/// Retrieves the Exception Syndrome Register (ESR) value from EL2.
///
/// # Returns
/// The value of the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_esr() -> usize {
    ESR_EL2.get() as usize
}

/// Reads the Exception Class (EC) field from the ESR_EL2 register.
///
/// # Returns
/// An `Option` containing the enum value representing the exception class.
#[inline(always)]
pub fn exception_class() -> Option<ESR_EL2::EC::Value> {
    ESR_EL2.read_as_enum(ESR_EL2::EC)
}

/// Reads the Exception Class (EC) field from the ESR_EL2 register and returns it as a raw value.
///
/// # Returns
/// The value of the EC field in the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_class_value() -> usize {
    ESR_EL2.read(ESR_EL2::EC) as usize
}

/// Retrieves the Hypervisor IPA Fault Address Register (HPFAR) value from EL2.
///
/// This function uses inline assembly to read the HPFAR_EL2 register.
///
/// # Returns
/// The value of the HPFAR_EL2 register as a `usize`.
#[inline(always)]
fn exception_hpfar() -> usize {
    let hpfar: u64;
    unsafe {
        core::arch::asm!("mrs {}, HPFAR_EL2", out(reg) hpfar);
    }
    hpfar as usize
}

/// Macro for executing an ARM Address Translation (AT) instruction.
///
/// The macro takes two arguments:
/// - `$at_op`: The AT operation to perform (e.g., `"s1e1r"`).
/// - `$addr`: The address on which to perform the AT operation.
///
/// This macro is unsafe because it directly executes assembly code.
///
/// Example usage:
/// ```ignore
/// arm_at!("s1e1r", address);
/// ```
macro_rules! arm_at {
    ($at_op:expr, $addr:expr) => {
        unsafe {
            core::arch::asm!(concat!("AT ", $at_op, ", {0}"), in(reg) $addr, options(nomem, nostack));
            core::arch::asm!("isb");
        }
    };
}

/// Translates a Fault Address Register (FAR) to a Hypervisor Physical Fault Address Register (HPFAR).
///
/// This function uses the ARM Address Translation (AT) instruction to translate
/// the provided FAR to an HPFAR. The translation result is returned in the Physical
/// Address Register (PAR_EL1), and is then converted to the HPFAR format using the
/// `par_to_far` function.
///
/// # Arguments
/// * `far` - The Fault Address Register value that needs to be translated.
///
/// # Returns
/// * [`ArmVcpuResult<usize>`] - The translated HPFAR value, or an error if translation fails.
///
/// # Errors
/// Returns a `BadState` error if the translation is aborted (indicated by the `F` bit in `PAR_EL1`).
fn translate_far_to_hpfar(far: usize) -> ArmVcpuResult<usize> {
    // We have
    // 	PAR[PA_Shift - 1 : 12] = PA[PA_Shift - 1 : 12]
    // 	HPFAR[PA_Shift - 9 : 4]  = FIPA[PA_Shift - 1 : 12]
    // #define PAR_TO_HPFAR(par) (((par) & GENMASK_ULL(PHYS_MASK_SHIFT - 1, 12)) >> 8)
    fn par_to_far(par: u64) -> u64 {
        let mask = ((1 << (52 - 12)) - 1) << 12;
        (par & mask) >> 8
    }

    let par = PAR_EL1.get();
    arm_at!("s1e1r", far);
    let tmp = PAR_EL1.get();
    PAR_EL1.set(par);
    if (tmp & PAR_EL1::F::TranslationAborted.value) != 0 {
        Err(ArmVcpuError::BadState)
    } else {
        Ok(par_to_far(tmp) as usize)
    }
}

/// Captures the architecturally valid portion of the IPA for a data abort.
///
/// HPFAR validity follows the Arm conditions mirrored by KVM. If HPFAR was not
/// populated, this attempts an EL1 address translation only when FAR is safe to
/// use. A stage-1 table walk or invalid FAR deliberately yields only a page IPA
/// or no IPA instead of inventing a byte offset.
#[inline(always)]
pub fn exception_fault_ipa(syndrome: ArmDataAbortSyndrome) -> ArmVcpuResult<Option<ArmFaultIpa>> {
    let far = FAR_EL2.get();
    let hpfar = if syndrome.hpfar_is_valid() {
        exception_hpfar()
    } else if !syndrome.fault_safe_to_translate() {
        return Ok(None);
    } else {
        match translate_far_to_hpfar(far as usize) {
            Ok(hpfar) => hpfar,
            Err(ArmVcpuError::BadState) => return Ok(None),
            Err(error) => return Err(error),
        }
    };
    Ok(Some(ArmFaultIpa::from_hpfar(
        hpfar,
        far,
        syndrome.has_valid_ipa_offset(),
    )))
}

/// Determines the instruction length based on the ESR_EL2 register.
///
/// # Returns
/// - `1` if the instruction is 32-bit.
/// - `0` if the instruction is 16-bit.
#[inline(always)]
fn exception_instruction_length() -> usize {
    (exception_esr() >> 25) & 1
}

/// Calculates the step size to the next instruction after an exception.
///
/// # Returns
/// The step size to the next instruction:
/// - `4` for a 32-bit instruction.
/// - `2` for a 16-bit instruction.
#[inline(always)]
pub fn exception_next_instruction_step() -> usize {
    2 + 2 * exception_instruction_length()
}

#[inline(always)]
pub fn exception_sysreg_direction_write(iss: u64) -> bool {
    const ESR_ISS_SYSREG_DIRECTION: u64 = 0b1;
    (iss & ESR_ISS_SYSREG_DIRECTION) == 0
}

#[inline(always)]
pub fn exception_sysreg_gpr(iss: u64) -> u64 {
    const ESR_ISS_SYSREG_REG_OFF: u64 = 5;
    const ESR_ISS_SYSREG_REG_LEN: u64 = 5;
    const ESR_ISS_SYSREG_REG_MASK: u64 = (1 << ESR_ISS_SYSREG_REG_LEN) - 1;
    (iss >> ESR_ISS_SYSREG_REG_OFF) & ESR_ISS_SYSREG_REG_MASK
}

/// The numbering of `SystemReg` follows the order specified in the Instruction Set Specification (ISS),
/// formatted as `<op0><op2><op1><CRn>00000<CRm>0`.
/// (Op0[21..20] + Op2[19..17] + Op1[16..14] + CRn[13..10]) + CRm[4..1]
#[inline(always)]
pub const fn exception_sysreg_addr(iss: usize) -> usize {
    const ESR_ISS_SYSREG_ADDR: usize = (0xfff << 10) | (0xf << 1);
    iss & ESR_ISS_SYSREG_ADDR
}

/// Macro to save the host function context to the stack.
///
/// This macro saves the values of the callee-saved registers (`x19` to `x30`) to the stack.
/// The stack pointer (`sp`) is adjusted accordingly
/// to make space for the saved registers.
///
/// ## Note
///
/// This macro should be used in conjunction with `restore_regs_from_stack!` to ensure that
/// the saved registers are properly restored when needed,
/// and the control flow can be returned to `ArmVcpu::run()` in `vcpu.rs` happily.
macro_rules! save_regs_to_stack {
    () => {
        "
        sub     sp, sp, 12 * 8
        stp     x29, x30, [sp, 10 * 8]
        stp     x27, x28, [sp, 8 * 8]
        stp     x25, x26, [sp, 6 * 8]
        stp     x23, x24, [sp, 4 * 8]
        stp     x21, x22, [sp, 2 * 8]
        stp     x19, x20, [sp]"
    };
}

/// Macro to restore the host function context from the stack.
///
/// This macro restores the values of the callee-saved general-purpose registers (`x19` to `x30`) from the stack.
/// The stack pointer (`sp`) is adjusted back after restoring the registers.
///
/// ## Note
///
/// This macro is called in `return_run_guest()` in exception.rs,
/// it should only be used after `save_regs_to_stack!` to correctly restore the control flow of `ArmVcpu::run()`.
macro_rules! restore_regs_from_stack {
    () => {
        "
        ldp     x19, x20, [sp]
        ldp     x21, x22, [sp, 2 * 8]
        ldp     x23, x24, [sp, 4 * 8]
        ldp     x25, x26, [sp, 6 * 8]
        ldp     x27, x28, [sp, 8 * 8]
        ldp     x29, x30, [sp, 10 * 8]
        add     sp, sp, 12 * 8"
    };
}
