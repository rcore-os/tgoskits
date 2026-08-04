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

use aarch64_cpu::registers::{ESR_EL2, HCR_EL2, Readable, SCTLR_EL1, VTCR_EL2, VTTBR_EL2};
use log::error;

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
use crate::{ArmAccessWidth, ArmSysRegAddr, ArmVcpuError, ArmVcpuResult, ArmVmExit};

numeric_enum_macro::numeric_enum! {
#[repr(u8)]
#[derive(Debug)]
pub enum TrapKind {
    Synchronous = 0,
    Irq = 1,
    Fiq = 2,
    SError = 3,
}
}

/// Equals to [`TrapKind::Synchronous`], used in exception.S.
const EXCEPTION_SYNC: usize = TrapKind::Synchronous as usize;
/// Equals to [`TrapKind::Irq`], used in exception.S.
const EXCEPTION_IRQ: usize = TrapKind::Irq as usize;

const AARCH64_EXCEPTION_INSN_SIZE: usize = 4;

fn advance_aarch64_exception_pc(ctx: &mut TrapFrame) {
    ctx.set_exception_pc(ctx.exception_pc() + AARCH64_EXCEPTION_INSN_SIZE);
}

#[repr(u8)]
#[derive(Debug)]
#[allow(unused)]
enum TrapSource {
    CurrentSpEl0 = 0,
    CurrentSpElx = 1,
    LowerAArch64 = 2,
    LowerAArch32 = 3,
}

core::arch::global_asm!(
    include_str!("exception.S"),
    exception_sync = const EXCEPTION_SYNC,
    exception_irq = const EXCEPTION_IRQ,
    trap_frame_size = const crate::ARM_VCPU_TRAP_FRAME_SIZE,
    guest_tpidr_el0_offset = const super::vcpu::ARM_VCPU_GUEST_TPIDR_EL0_OFFSET,
    host_tpidr_el0_offset = const super::vcpu::ARM_VCPU_HOST_TPIDR_EL0_OFFSET,
    host_irq_interface_offset = const super::vcpu::ARM_VCPU_HOST_IRQ_INTERFACE_OFFSET,
    host_irq_cpu_interface_base_offset =
        const super::vcpu::ARM_VCPU_HOST_IRQ_CPU_INTERFACE_BASE_OFFSET,
    host_pending_irq_ack_offset = const super::vcpu::ARM_VCPU_HOST_PENDING_IRQ_ACK_OFFSET,
    host_irq_interface_gicv2_mmio = const super::host::HOST_IRQ_INTERFACE_GICV2_MMIO,
    host_irq_interface_gicv3_sysreg = const super::host::HOST_IRQ_INTERFACE_GICV3_SYSREG,
    timer_virtual_offset_offset = const super::vcpu::ARM_VCPU_TIMER_VIRTUAL_OFFSET_OFFSET,
    timer_virtual_compare_offset = const super::vcpu::ARM_VCPU_TIMER_VIRTUAL_COMPARE_OFFSET,
    timer_virtual_control_offset = const super::vcpu::ARM_VCPU_TIMER_VIRTUAL_CONTROL_OFFSET,
    timer_guest_hypervisor_control_offset =
        const super::vcpu::ARM_VCPU_TIMER_GUEST_HYPERVISOR_CONTROL_OFFSET,
    timer_guest_kernel_control_offset =
        const super::vcpu::ARM_VCPU_TIMER_GUEST_KERNEL_CONTROL_OFFSET,
    timer_host_hypervisor_control_offset =
        const super::vcpu::ARM_VCPU_TIMER_HOST_HYPERVISOR_CONTROL_OFFSET,
    timer_host_kernel_control_offset =
        const super::vcpu::ARM_VCPU_TIMER_HOST_KERNEL_CONTROL_OFFSET,
    timer_loaded_offset = const super::vcpu::ARM_VCPU_TIMER_LOADED_OFFSET,
);

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
pub fn handle_exception_sync(ctx: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    match exception_class() {
        Some(ESR_EL2::EC::Value::TrappedWFIorWFE) => {
            let next_pc = ctx.exception_pc() + exception_next_instruction_step();
            ctx.set_exception_pc(next_pc);
            Ok(ArmVmExit::WaitForInterrupt)
        }
        Some(ESR_EL2::EC::Value::DataAbortLowerEL) => {
            let elr = ctx.exception_pc();
            let val = elr + exception_next_instruction_step();
            ctx.set_exception_pc(val);
            handle_data_abort(ctx)
        }
        Some(ESR_EL2::EC::Value::HVC64) => {
            // The `#imm` argument when triggering a hvc call, currently not used.
            let _hvc_arg_imm16 = ESR_EL2.read(ESR_EL2::ISS);

            if let Some(result) = handle_hvc_psci_version(ctx) {
                return result;
            }

            handle_hvc64_exception(ctx)
        }
        Some(ESR_EL2::EC::Value::TrappedMsrMrs) => handle_system_register(ctx),
        Some(ESR_EL2::EC::Value::SMC64) => {
            let elr = ctx.exception_pc();
            let val = elr + exception_next_instruction_step();
            ctx.set_exception_pc(val);
            handle_smc64_exception(ctx)
        }
        _ => {
            panic!(
                "handler not presents for EC_{} @ipa 0x{:x}, @pc 0x{:x}, @esr 0x{:x},
                @sctlr_el1 0x{:x}, @vttbr_el2 0x{:x}, @vtcr_el2: {:#x} hcr: {:#x} ctx:{}",
                exception_class_value(),
                exception_fault_addr()?,
                (*ctx).exception_pc(),
                exception_esr(),
                SCTLR_EL1.get() as usize,
                VTTBR_EL2.get() as usize,
                VTCR_EL2.get() as usize,
                HCR_EL2.get() as usize,
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

    advance_aarch64_exception_pc(ctx);
    ctx.set_gpr(0, PSCI_VERSION_0_2);
    Some(Ok(ArmVmExit::Nothing))
}

fn handle_hvc64_exception(ctx: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    advance_aarch64_exception_pc(ctx);

    // Is this a psci call?
    //
    // By convention, a psci call can use either the `hvc` or the `smc` instruction.
    // NimbOS uses `hvc`, `ArceOS` use `hvc` too when running on QEMU.
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

fn handle_data_abort(context_frame: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    let addr = exception_fault_addr()?;
    let access_width = exception_data_abort_access_width();
    let is_write = exception_data_abort_access_is_write();
    // let sign_ext = exception_data_abort_access_is_sign_ext();
    let reg = exception_data_abort_access_reg();
    let reg_width = exception_data_abort_access_reg_width();

    trace!(
        "Data fault @{:?}, ELR {:#x}, esr: 0x{:x}",
        addr,
        context_frame.exception_pc(),
        exception_esr(),
    );

    let width = ArmAccessWidth::try_from(access_width)?;
    let reg_width = ArmAccessWidth::try_from(reg_width)?;

    if !exception_data_abort_handleable() {
        panic!(
            "Core data abort not handleable {:#x}, esr {:#x}",
            addr,
            exception_esr()
        );
    }

    if !exception_data_abort_is_translate_fault() {
        if exception_data_abort_is_permission_fault() {
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
fn handle_system_register(context_frame: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    let iss = ESR_EL2.read(ESR_EL2::ISS);

    let addr = exception_sysreg_addr(iss.try_into().unwrap());
    let elr = context_frame.exception_pc();
    let val = elr + exception_next_instruction_step();
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
    // Is this a psci call?
    if let Some(result) = handle_psci_call(ctx) {
        result
    } else {
        // We just forward the SMC call to the ATF directly.
        // The args are from lower EL, so it is safe to call the ATF.
        (ctx.gpr[0], ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]) =
            unsafe { super::smc::smc_call(ctx.gpr[0], ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]) };
        Ok(ArmVmExit::Nothing)
    }
}

/// Handles IRQ exceptions that occur from the current exception level.
/// Dispatches IRQs to the appropriate handler provided by the underlying host OS,
/// which is provided by the host callback.
#[unsafe(no_mangle)]
fn current_el_irq_handler(_tf: &mut TrapFrame) {
    // TODO: consider if returning VmExit::ExternalInterrupt (or another enum variant) is
    // better than directly calling the handler here.
    super::host::handle_current_host_irq()
}

/// Handles synchronous exceptions that occur from the current exception level.
#[unsafe(no_mangle)]
fn current_el_sync_handler(tf: &mut TrapFrame) {
    let esr = ESR_EL2.extract();
    let ec = ESR_EL2.read(ESR_EL2::EC);
    let iss = ESR_EL2.read(ESR_EL2::ISS);

    error!("ESR_EL2: {:#x}", esr.get());
    error!("Exception Class: {ec:#x}");
    error!("Instruction Specific Syndrome: {iss:#x}");

    panic!(
        "Unhandled synchronous exception from current EL: {:#x?}",
        tf
    );
}

/// A trampoline function for sp switching during handling VM exits,
/// when **there is a active VCPU running**, which means that the host context is stored
/// into host stack in `run_guest` function.
///
/// # Functionality
///
/// 1. **Restore Previous Host Stack pointor:**
///     - The guest context frame is aleady saved by `SAVE_REGS_FROM_EL1` macro in exception.S.
///       This function firstly adjusts the `sp` to skip the exception frame
///       according to the memory layout of [`crate::ArmVcpu`], which makes current `sp`
///       point to the address of `host.stack_top`.
///       The saved host `SP_EL0` is restored before any host Rust runs again, then
///       the host stack top value is restored by `ldr`.
///
/// 2. **Restore Host Context:**
///     - The `restore_regs_from_stack!()` macro is invoked to restore the host function context
///       from the stack. This macro handles the restoration of the host's callee-saved general-purpose
///       registers (`x19` to `x30`).
///
/// 3. **Restore Host Control Flow:**
///     - The `ret` instruction is used to return control to the host context after
///       the guest context has been saved in `ArmVcpu` and the host context restored.
///       Finally the control flow is returned back to `ArmVcpu::run()` in [vcpu.rs].
///
/// # Notes
///
/// - This function is typically invoked when a VM exit occurs, requiring the
///   hypervisor to switch context from the guest to the host. The precise control
///   over stack and register management ensures that the transition is smooth and
///   that the host can correctly resume execution.
///
/// - The `options(noreturn)` directive indicates that this function will not return
///   to its caller, as control will be transferred back to the host context via `ret`.
///
/// - This function is not typically called directly from Rust code. Instead, it is
///   invoked as part of the low-level hypervisor or VM exit handling routines.
#[unsafe(naked)]
#[unsafe(no_mangle)]
unsafe extern "C" fn vmexit_trampoline() -> ! {
    core::arch::naked_asm!(
        // Currently `sp` points to the base address of `ArmVcpu.ctx`, which stores guest's `TrapFrame`.
        "add x9, sp, {host_stack_top_offset}", // Skip the exception frame.
        // Currently `x9` points to `&ArmVcpu.host.stack_top`, see `run_guest()` in vcpu.rs.
        "ldr x11, [x9, {host_sp_el0_delta}]", // Restore host SP_EL0 before host Rust resumes.
        "msr sp_el0, x11",
        "ldr x10, [x9]", // Get `host_stack_top` value from `&ArmVcpu.host.stack_top`.
        "mov sp, x10",   // Set `sp` as the host stack top.
        restore_regs_from_stack!(), // Restore host function context frame.
        "ret", /* Control flow is handed back to ArmVcpu::run(), simulating the normal return of the `run_guest` function. */
        host_stack_top_offset = const crate::ARM_VCPU_HOST_STACK_TOP_OFFSET,
        host_sp_el0_delta = const crate::ARM_VCPU_HOST_SP_EL0_OFFSET - crate::ARM_VCPU_HOST_STACK_TOP_OFFSET,
    )
}

/// Deal with invalid aarch64 exception.
#[unsafe(no_mangle)]
fn invalid_exception_el2(tf: &mut TrapFrame, kind: TrapKind, source: TrapSource) {
    panic!(
        "Invalid exception {:?} from {:?}:\n{:#x?}",
        kind, source, tf
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const PSCI_VERSION_32: u64 = 0x8400_0000;
    const GENERIC_HVC_NR: u64 = 0x1234_5678;
    const TEST_PC: usize = 0x8020_0000;

    #[test]
    fn hvc_psci_exit_advances_exception_pc() {
        let mut ctx = TrapFrame::default();
        ctx.set_exception_pc(TEST_PC);
        ctx.set_gpr(0, PSCI_VERSION_32 as usize);

        let exit = handle_hvc64_exception(&mut ctx).expect("PSCI HVC should produce VM exit");

        assert_eq!(ctx.exception_pc(), TEST_PC + AARCH64_EXCEPTION_INSN_SIZE);
        assert!(matches!(
            exit,
            ArmVmExit::Hypercall {
                nr: PSCI_VERSION_32,
                ..
            }
        ));
    }

    #[test]
    fn generic_hvc_exit_advances_exception_pc() {
        let mut ctx = TrapFrame::default();
        ctx.set_exception_pc(TEST_PC);
        ctx.set_gpr(0, GENERIC_HVC_NR as usize);
        ctx.set_gpr(1, 1);
        ctx.set_gpr(2, 2);

        let exit = handle_hvc64_exception(&mut ctx).expect("generic HVC should produce VM exit");

        assert_eq!(ctx.exception_pc(), TEST_PC + AARCH64_EXCEPTION_INSN_SIZE);
        assert!(matches!(
            exit,
            ArmVmExit::Hypercall {
                nr: GENERIC_HVC_NR,
                args: [1, 2, _, _, _, _],
            }
        ));
    }
}
