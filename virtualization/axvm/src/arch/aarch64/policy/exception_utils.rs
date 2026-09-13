use aarch64_cpu::registers::ESR_EL2;
use ax_cpu::virtualization::Exit;
use tock_registers::fields::TryFromValue;

use crate::arch::aarch64::policy::{ArmGuestPhysAddr, ArmVcpuError, ArmVcpuResult};

/// Retrieves the Exception Syndrome Register (ESR) value from EL2.
///
/// # Returns
/// The value of the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_esr(exit: &Exit) -> usize {
    exit.syndrome as usize
}

/// Reads the Exception Class (EC) field from the ESR_EL2 register.
///
/// # Returns
/// An `Option` containing the enum value representing the exception class.
#[inline(always)]
pub fn exception_class(exit: &Exit) -> Option<ESR_EL2::EC::Value> {
    ESR_EL2::EC::Value::try_from_value((exit.syndrome >> 26) & 63)
}

/// Reads the Exception Class (EC) field from the ESR_EL2 register and returns it as a raw value.
///
/// # Returns
/// The value of the EC field in the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_class_value(exit: &Exit) -> usize {
    ((exit.syndrome >> 26) & 63) as usize
}

/// Uses the IPA resolved by the CPU before its host register-bank restoration.
pub fn exception_fault_addr(exit: &Exit) -> ArmVcpuResult<ArmGuestPhysAddr> {
    exit.guest_address
        .map(|address| ArmGuestPhysAddr::from_usize(address.as_usize()))
        .map_err(|_| ArmVcpuError::BadState)
}

/// Determines the instruction length based on the ESR_EL2 register.
///
/// # Returns
/// - `1` if the instruction is 32-bit.
/// - `0` if the instruction is 16-bit.
#[inline(always)]
fn exception_instruction_length(exit: &Exit) -> usize {
    (exception_esr(exit) >> 25) & 1
}

/// Calculates the step size to the next instruction after an exception.
///
/// # Returns
/// The step size to the next instruction:
/// - `4` for a 32-bit instruction.
/// - `2` for a 16-bit instruction.
#[inline(always)]
pub fn exception_next_instruction_step(exit: &Exit) -> usize {
    2 + 2 * exception_instruction_length(exit)
}

/// Retrieves the Instruction Specific Syndrome (ISS) field from the ESR_EL2 register.
///
/// # Returns
/// The value of the ISS field in the ESR_EL2 register as a `usize`.
#[inline(always)]
pub fn exception_iss(exit: &Exit) -> usize {
    (exit.syndrome & 0x1ff_ffff) as usize
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

/// Checks if the data abort exception was caused by a permission fault.
///
/// # Returns
/// - `true` if the exception was caused by a permission fault.
/// - `false` otherwise.
#[inline(always)]
pub fn exception_data_abort_is_permission_fault(exit: &Exit) -> bool {
    (exception_iss(exit) & 0b111111 & (0xf << 2)) == 12
}

/// Determines the access width of a data abort exception.
///
/// # Returns
/// The access width in bytes (1, 2, 4, or 8 bytes).
#[inline(always)]
pub fn exception_data_abort_access_width(exit: &Exit) -> usize {
    1 << ((exception_iss(exit) >> 22) & 0b11)
}

/// Determines the DA can be handled
#[inline(always)]
pub fn exception_data_abort_handleable(exit: &Exit) -> bool {
    (!(exception_iss(exit) & (1 << 10)) | (exception_iss(exit) & (1 << 24))) != 0
}

#[inline(always)]
pub fn exception_data_abort_is_translate_fault(exit: &Exit) -> bool {
    (exception_iss(exit) & 0b111111 & (0xf << 2)) == 4
}

/// Checks if the data abort exception was caused by a write access.
///
/// # Returns
/// - `true` if the exception was caused by a write access.
/// - `false` if it was caused by a read access.
#[inline(always)]
pub fn exception_data_abort_access_is_write(exit: &Exit) -> bool {
    (exception_iss(exit) & (1 << 6)) != 0
}

/// Retrieves the register index involved in a data abort exception.
///
/// # Returns
/// The index of the register (0-31) involved in the access.
#[inline(always)]
pub fn exception_data_abort_access_reg(exit: &Exit) -> usize {
    (exception_iss(exit) >> 16) & 0b11111
}

/// Determines the width of the register involved in a data abort exception.
///
/// # Returns
/// The width of the register in bytes (4 or 8 bytes).
#[allow(unused)]
#[inline(always)]
pub fn exception_data_abort_access_reg_width(exit: &Exit) -> usize {
    4 + 4 * ((exception_iss(exit) >> 15) & 1)
}

/// Checks if the data accessed during a data abort exception is sign-extended.
///
/// # Returns
/// - `true` if the data is sign-extended.
/// - `false` otherwise.
#[allow(unused)]
#[inline(always)]
pub fn exception_data_abort_access_is_sign_ext(exit: &Exit) -> bool {
    ((exception_iss(exit) >> 21) & 1) != 0
}
