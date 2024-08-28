use aarch64_cpu::registers::{ESR_EL2, FAR_EL2, PAR_EL1};
use tock_registers::interfaces::*;

use axaddrspace::GuestPhysAddr;
use axerrno::{ax_err, AxResult};

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

/// Constant for the shift amount used to identify the S1PTW bit in ESR_ELx.
#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW_SHIFT: usize = 7;
/// Constant representing the S1PTW (Stage 1 translation fault) bit in ESR_ELx.
#[allow(non_upper_case_globals)]
const ESR_ELx_S1PTW: usize = 1 << ESR_ELx_S1PTW_SHIFT;

/// Macro for executing an ARM Address Translation (AT) instruction.
///
/// The macro takes two arguments:
/// - `$at_op`: The AT operation to perform (e.g., `"s1e1r"`).
/// - `$addr`: The address on which to perform the AT operation.
///
/// This macro is unsafe because it directly executes assembly code.
///
/// Example usage:
/// ```rust
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
/// * `AxResult<usize>` - The translated HPFAR value, or an error if translation fails.
///
/// # Errors
/// Returns a `BadState` error if the translation is aborted (indicated by the `F` bit in `PAR_EL1`).
fn translate_far_to_hpfar(far: usize) -> AxResult<usize> {
    /*
     * We have
     *	PAR[PA_Shift - 1 : 12] = PA[PA_Shift - 1 : 12]
     *	HPFAR[PA_Shift - 9 : 4]  = FIPA[PA_Shift - 1 : 12]
     */
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
        ax_err!(BadState, "PAR_EL1::F::TranslationAborted value")
    } else {
        Ok(par_to_far(tmp) as usize)
    }
}

/// Retrieves the fault address that caused an exception.
///
/// This function returns the Guest Physical Address (GPA) that caused the
/// exception. The address is determined based on the `FAR_EL2` and `HPFAR_EL2`
/// registers. If the exception is not due to a permission fault or if stage 1
/// translation is involved, the function uses `HPFAR_EL2` to compute the final
/// address.
///
/// - `far` is the Fault Address Register (FAR_EL2) value.
/// - `hpfar` is the Hypervisor Fault Address Register (HPFAR_EL2) value,
///   which might be derived from `FAR_EL2` if certain conditions are met.
///
/// The final address returned is computed by combining the page offset from
/// `FAR_EL2` with the page number from `HPFAR_EL2`.
///
/// # Returns
/// * `AxResult<GuestPhysAddr>` - The guest physical address that caused the exception, wrapped in an `AxResult`.
#[inline(always)]
pub fn exception_fault_addr() -> AxResult<GuestPhysAddr> {
    let far = FAR_EL2.get() as usize;
    let hpfar =
        if (exception_esr() & ESR_ELx_S1PTW) == 0 && exception_data_abort_is_permission_fault() {
            translate_far_to_hpfar(far)?
        } else {
            exception_hpfar()
        };
    Ok(GuestPhysAddr::from((far & 0xfff) | (hpfar << 8)))
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

/// Retrieves the Instruction Specific Syndrome (ISS) field from the ESR_EL2 register.
///
/// # Returns
/// The value of the ISS field in the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_iss() -> usize {
    ESR_EL2.read(ESR_EL2::ISS) as usize
}

/// Checks if the data abort exception was caused by a permission fault.
///
/// # Returns
/// - `true` if the exception was caused by a permission fault.
/// - `false` otherwise.
#[inline(always)]
pub fn exception_data_abort_is_permission_fault() -> bool {
    (exception_iss() & 0b111111 & (0xf << 2)) == 12
}

/// Determines the access width of a data abort exception.
///
/// # Returns
/// The access width in bytes (1, 2, 4, or 8 bytes).
#[inline(always)]
pub fn exception_data_abort_access_width() -> usize {
    1 << ((exception_iss() >> 22) & 0b11)
}

/// Checks if the data abort exception was caused by a write access.
///
/// # Returns
/// - `true` if the exception was caused by a write access.
/// - `false` if it was caused by a read access.
#[inline(always)]
pub fn exception_data_abort_access_is_write() -> bool {
    (exception_iss() & (1 << 6)) != 0
}

/// Retrieves the register index involved in a data abort exception.
///
/// # Returns
/// The index of the register (0-31) involved in the access.
#[inline(always)]
pub fn exception_data_abort_access_reg() -> usize {
    (exception_iss() >> 16) & 0b11111
}

/// Determines the width of the register involved in a data abort exception.
///
/// # Returns
/// The width of the register in bytes (4 or 8 bytes).
#[allow(unused)]
#[inline(always)]
pub fn exception_data_abort_access_reg_width() -> usize {
    4 + 4 * ((exception_iss() >> 15) & 1)
}

/// Checks if the data accessed during a data abort exception is sign-extended.
///
/// # Returns
/// - `true` if the data is sign-extended.
/// - `false` otherwise.
#[allow(unused)]
#[inline(always)]
pub fn exception_data_abort_access_is_sign_ext() -> bool {
    ((exception_iss() >> 21) & 1) != 0
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
/// and the control flow can be returned to `Aarch64VCpu.run()` in `vcpu.rs` happily.
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
/// This macro is called in `vmexit_trampoline()` in exception.rs,
/// it should only be used after `save_regs_to_stack!` to correctly restore the control flow of `Aarch64VCpu.run()`.
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
