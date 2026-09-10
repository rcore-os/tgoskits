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

use aarch64_cpu::registers::ESR_EL2;
pub use ax_cpu::virtualization::ExitKind as TrapKind;
use ax_cpu::virtualization::{Exit, GuestSystemRegisters};

use super::{
    TrapFrame,
    exception_utils::{
        exception_class, exception_class_value, exception_data_abort_access_is_write,
        exception_data_abort_access_reg, exception_data_abort_access_reg_width,
        exception_data_abort_access_width, exception_data_abort_handleable,
        exception_data_abort_is_permission_fault, exception_data_abort_is_translate_fault,
        exception_esr, exception_fault_addr, exception_next_instruction_step,
        exception_sysreg_addr, exception_sysreg_direction_write, exception_sysreg_gpr,
    },
};
use crate::arch::aarch64::policy::{
    ArmAccessWidth, ArmSysRegAddr, ArmVcpuError, ArmVcpuResult, ArmVmExit,
};

/// Handles synchronous exceptions that occur during the execution of a guest VM.
///
/// This function examines the exception class (EC) to determine the cause of the exception
/// and then handles it accordingly.
///
/// Currently we just handle exception type including data abort (`DataAbortLowerEL`) and hypervisor call (`HVC64)`.
///
/// # Arguments
///
/// * `ctx` - A mutable reference to the `TrapFrame`, which contains the saved state of the guest VM's CPU registers at the time of the exception.
///
/// # Returns
///
/// An [`ArmVcpuResult`] containing an [`ArmVmExit`] indicating the reason for the VM exit.
/// This could be due to a hypervisor call (`Hypercall`) or other reasons such as data aborts.
///
/// # Panics
///
/// If an unhandled exception class is encountered, the function will panic, outputting
/// details about the exception including the instruction pointer, faulting address, exception
/// syndrome register (ESR), and system control registers.
pub fn handle_exception_sync(
    ctx: &mut TrapFrame,
    system: &GuestSystemRegisters,
    exit: &Exit,
) -> ArmVcpuResult<ArmVmExit> {
    match exception_class(exit) {
        Some(ESR_EL2::EC::Value::TrappedWFIorWFE) => {
            let next_pc = ctx.exception_pc() + exception_next_instruction_step(exit);
            ctx.set_exception_pc(next_pc);
            Ok(ArmVmExit::WaitForInterrupt)
        }
        Some(ESR_EL2::EC::Value::DataAbortLowerEL) => {
            let elr = ctx.exception_pc();
            let val = elr + exception_next_instruction_step(exit);
            ctx.set_exception_pc(val);
            handle_data_abort(ctx, exit)
        }
        Some(ESR_EL2::EC::Value::HVC64) => {
            // HVC records the preferred return address (the instruction after
            // `hvc`) in ELR_EL2, so the handlers must preserve this PC.
            // The `#imm` argument when triggering a hvc call, currently not used.
            let _hvc_arg_imm16 = exit.syndrome & 0x1ff_ffff;

            if let Some(result) = handle_hvc_psci_version(ctx) {
                return result;
            }

            handle_hvc64_exception(ctx)
        }
        Some(ESR_EL2::EC::Value::TrappedMsrMrs) => handle_system_register(ctx, exit),
        Some(ESR_EL2::EC::Value::SMC64) => {
            // An SMC trapped by HCR_EL2.TSC is a Trap exception, whose
            // preferred return address is the `smc` itself. Advance past it
            // before resuming the guest.
            let elr = ctx.exception_pc();
            let val = elr + exception_next_instruction_step(exit);
            ctx.set_exception_pc(val);
            handle_smc64_exception(ctx)
        }
        _ => {
            panic!(
                "handler not presents for EC_{} @ipa 0x{:x}, @pc 0x{:x}, @esr 0x{:x},
                @sctlr_el1 0x{:x}, @vttbr_el2 0x{:x}, @vtcr_el2: {:#x} hcr: {:#x} ctx:{}",
                exception_class_value(exit),
                exception_fault_addr(exit)?,
                (*ctx).exception_pc(),
                exception_esr(exit),
                system.sctlr_el1 as usize,
                system.vttbr_el2 as usize,
                system.vtcr_el2 as usize,
                system.hcr_el2 as usize,
                ctx
            );
        }
    }
}

fn handle_hvc_psci_version(ctx: &mut TrapFrame) -> Option<ArmVcpuResult<ArmVmExit>> {
    const PSCI_VERSION_32: u64 = 0x8400_0000;
    const PSCI_VERSION_0_2: usize = 0x0000_0002;

    if ctx.gpr[0] != PSCI_VERSION_32 {
        return None;
    }

    ctx.set_gpr(0, PSCI_VERSION_0_2);
    Some(Ok(ArmVmExit::Nothing))
}

fn handle_hvc64_exception(ctx: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    // The low-level AArch64 trap entry already saves the guest return PC for
    // HVC exits. Advancing it here would skip the instruction after `hvc`.
    // Is this a psci call?
    //
    // By convention, a psci call can use either the `hvc` or the `smc` instruction.
    // ArceOS and other QEMU guests use `hvc` for hypercalls.
    if let Some(result) = handle_psci_call(ctx) {
        return result;
    }

    // We assume that guest VM triggers HVC through a `hvc #0` instruction.
    // And arm64 hcall implementation uses `x0` to specify the hcall number.
    Ok(ArmVmExit::Hypercall {
        nr: ctx.gpr[0],
        args: [
            ctx.gpr[1], ctx.gpr[2], ctx.gpr[3], ctx.gpr[4], ctx.gpr[5], ctx.gpr[6],
        ],
    })
}

fn handle_data_abort(context_frame: &mut TrapFrame, exit: &Exit) -> ArmVcpuResult<ArmVmExit> {
    let addr = exception_fault_addr(exit)?;
    let access_width = exception_data_abort_access_width(exit);
    let is_write = exception_data_abort_access_is_write(exit);
    // let sign_ext = exception_data_abort_access_is_sign_ext();
    let reg = exception_data_abort_access_reg(exit);
    let reg_width = exception_data_abort_access_reg_width(exit);

    trace!(
        "Data fault @{:?}, ELR {:#x}, esr: 0x{:x}",
        addr,
        context_frame.exception_pc(),
        exception_esr(exit),
    );

    let width = ArmAccessWidth::try_from(access_width)?;
    let reg_width = ArmAccessWidth::try_from(reg_width)?;

    if !exception_data_abort_handleable(exit) {
        panic!(
            "Core data abort not handleable {:#x}, esr {:#x}",
            addr,
            exception_esr(exit)
        );
    }

    if !exception_data_abort_is_translate_fault(exit) {
        if exception_data_abort_is_permission_fault(exit) {
            return Err(ArmVcpuError::Unsupported);
        } else {
            panic!("Core data abort is not translate fault {:#x}", addr,);
        }
    }

    if is_write {
        return Ok(ArmVmExit::MmioWrite {
            addr,
            width,
            data: context_frame.gpr(reg) as u64,
        });
    }
    Ok(ArmVmExit::MmioRead {
        addr,
        width,
        reg,
        reg_width,
        signed_ext: false,
    })
}

/// Handles a system register access exception.
///
/// This function processes the exception by reading or writing to a system register
/// based on the information in the `context_frame`.
///
/// # Arguments
/// * `context_frame` - A mutable reference to the trap frame containing the CPU state.
///
/// # Returns
/// * [`ArmVcpuResult<ArmVmExit>`] - The VM-exit reason or a typed vCPU error.
///   whether the operation was a read or write and the relevant details.
fn handle_system_register(context_frame: &mut TrapFrame, exit: &Exit) -> ArmVcpuResult<ArmVmExit> {
    let iss = exit.syndrome & 0x1ff_ffff;

    let addr = exception_sysreg_addr(iss.try_into().unwrap());
    let elr = context_frame.exception_pc();
    let val = elr + exception_next_instruction_step(exit);
    let write = exception_sysreg_direction_write(iss);
    let reg = exception_sysreg_gpr(iss) as usize;
    context_frame.set_exception_pc(val);
    if write {
        return Ok(ArmVmExit::SysRegWrite {
            addr: ArmSysRegAddr::new(addr),
            value: context_frame.gpr(reg) as u64,
        });
    }
    Ok(ArmVmExit::SysRegRead {
        addr: ArmSysRegAddr::new(addr),
        reg,
    })
}

/// Handles HVC or SMC exceptions that serve as PSCI calls.
///
/// PSCI calls are normalized into `ArmVmExit::Hypercall` so that PSCI
/// semantics live in `axvm::runtime::hvc` instead of being split between
/// the trap layer and the VM runtime.
fn handle_psci_call(ctx: &TrapFrame) -> Option<ArmVcpuResult<ArmVmExit>> {
    const PSCI_FN_RANGE_32: core::ops::RangeInclusive<u64> = 0x8400_0000..=0x8400_001F;
    const PSCI_FN_RANGE_64: core::ops::RangeInclusive<u64> = 0xC400_0000..=0xC400_001F;

    let fn_id = ctx.gpr[0];
    if !PSCI_FN_RANGE_32.contains(&fn_id) && !PSCI_FN_RANGE_64.contains(&fn_id) {
        return None;
    }

    Some(Ok(ArmVmExit::Hypercall {
        nr: fn_id,
        args: [
            ctx.gpr[1], ctx.gpr[2], ctx.gpr[3], ctx.gpr[4], ctx.gpr[5], ctx.gpr[6],
        ],
    }))
}

/// Handles SMC (Secure Monitor Call) exceptions.
///
/// This function will judge if the SMC call is a PSCI call, if so, it will handle it as a PSCI call.
/// Otherwise, it will forward the SMC call to the ATF directly.
fn handle_smc64_exception(ctx: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    const PSCI_VERSION_32: u64 = 0x8400_0000;

    // Is this a psci call?
    // Keep virtual CPU lifecycle calls inside AxVisor, but expose the physical
    // firmware's PSCI version to SMC guests. Linux uses that version to decide
    // whether it may query the SMCCC version needed by SCMI.
    if ctx.gpr[0] != PSCI_VERSION_32
        && let Some(result) = handle_psci_call(ctx)
    {
        return result;
    }

    // We just forward the SMC call to the ATF directly.
    // The args are from lower EL, so it is safe to call the ATF.
    (ctx.gpr[0], ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]) =
        unsafe { super::smc::smc_call(ctx.gpr[0], ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]) };
    Ok(ArmVmExit::Nothing)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PSCI_VERSION_32: u64 = 0x8400_0000;
    const GENERIC_HVC_NR: u64 = 0x1234_5678;
    const TEST_PC: usize = 0x8020_0000;

    #[test]
    fn hvc_psci_version_preserves_exception_pc() {
        let mut ctx = TrapFrame::default();
        ctx.set_exception_pc(TEST_PC);
        ctx.set_gpr(0, PSCI_VERSION_32 as usize);

        let exit = handle_hvc_psci_version(&mut ctx)
            .expect("PSCI version HVC should produce a result")
            .expect("PSCI version HVC should be handled");

        assert_eq!(ctx.exception_pc(), TEST_PC);
        assert_eq!(ctx.gpr[0], 0x2);
        assert!(matches!(exit, ArmVmExit::Nothing));
    }

    #[test]
    fn generic_hvc_exit_preserves_exception_pc() {
        let mut ctx = TrapFrame::default();
        ctx.set_exception_pc(TEST_PC);
        ctx.set_gpr(0, GENERIC_HVC_NR as usize);
        ctx.set_gpr(1, 1);
        ctx.set_gpr(2, 2);

        let exit = handle_hvc64_exception(&mut ctx).expect("generic HVC should produce VM exit");

        assert_eq!(ctx.exception_pc(), TEST_PC);
        assert!(matches!(
            exit,
            ArmVmExit::Hypercall {
                nr: GENERIC_HVC_NR,
                args: [1, 2, _, _, _, _],
            }
        ));
    }
}
