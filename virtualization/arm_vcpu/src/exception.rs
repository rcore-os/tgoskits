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

use aarch64_cpu::registers::{ESR_EL2, FAR_EL2, HCR_EL2, Readable, SCTLR_EL1, VTCR_EL2, VTTBR_EL2};
use log::error;

use crate::{
    ArmDataAbort, ArmDataAbortSyndrome, ArmGuestPhysAddr, ArmGuestVirtAddr, ArmSysRegAddr,
    ArmVcpuResult, ArmVmExit, TrapFrame,
    data_abort::decode_data_access,
    exception_utils::{
        exception_class, exception_class_value, exception_esr, exception_fault_ipa,
        exception_next_instruction_step, exception_sysreg_addr, exception_sysreg_direction_write,
        exception_sysreg_gpr,
    },
};

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
/// Equals to [`TrapKind::Fiq`], used in exception.S.
const EXCEPTION_FIQ: usize = TrapKind::Fiq as usize;
/// Equals to [`TrapKind::SError`], used in exception.S.
const EXCEPTION_SERROR: usize = TrapKind::SError as usize;

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
    exception_fiq = const EXCEPTION_FIQ,
    exception_serror = const EXCEPTION_SERROR,
    trap_frame_size = const crate::ARM_VCPU_TRAP_FRAME_SIZE,
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
        Some(ESR_EL2::EC::Value::DataAbortLowerEL) => handle_data_abort(ctx),
        Some(ESR_EL2::EC::Value::HVC64) => {
            // The `#imm`` argument when triggering a hvc call, currently not used.
            let _hvc_arg_imm16 = ESR_EL2.read(ESR_EL2::ISS);

            // Is this a psci call?
            //
            // By convention, a psci call can use either the `hvc` or the `smc` instruction.
            // NimbOS uses `hvc`, `ArceOS` use `hvc` too when running on QEMU.
            if let Some(result) = handle_psci_call(ctx) {
                return result;
            }

            // We assume that guest VM triggers HVC through a `hvc #0`` instruction.
            // And arm64 hcall implementation uses `x0` to specify the hcall number.
            // For more details on the hypervisor call (HVC) mechanism and the use of general-purpose registers,
            // refer to the [Linux Kernel documentation on KVM ARM hypervisor ABI](https://github.com/torvalds/linux/blob/master/Documentation/virt/kvm/arm/hyp-abi.rst).
            Ok(ArmVmExit::Hypercall {
                nr: ctx.gpr[0],
                args: [
                    ctx.gpr[1], ctx.gpr[2], ctx.gpr[3], ctx.gpr[4], ctx.gpr[5], ctx.gpr[6],
                ],
            })
        }
        Some(ESR_EL2::EC::Value::TrappedMsrMrs) => handle_system_register(ctx),
        Some(ESR_EL2::EC::Value::SMC64) => {
            let elr = ctx.exception_pc();
            let val = elr + exception_next_instruction_step();
            ctx.set_exception_pc(val);
            handle_smc64_exception(ctx)
        }
        _ => {
            error!(
                "unsupported synchronous exception EC_{} at PC {:#x}, ESR {:#x}, SCTLR_EL1={:#x}, \
                 VTTBR_EL2={:#x}, VTCR_EL2={:#x}, HCR_EL2={:#x}; context: {}",
                exception_class_value(),
                (*ctx).exception_pc(),
                exception_esr(),
                SCTLR_EL1.get() as usize,
                VTTBR_EL2.get() as usize,
                VTCR_EL2.get() as usize,
                HCR_EL2.get() as usize,
                ctx
            );
            Err(crate::ArmVcpuError::Unsupported)
        }
    }
}

fn handle_data_abort(context_frame: &mut TrapFrame) -> ArmVcpuResult<ArmVmExit> {
    let syndrome = ArmDataAbortSyndrome::from_esr(exception_esr() as u32);
    let fault_ipa = match exception_fault_ipa(syndrome) {
        Ok(addr) => addr,
        Err(error) => {
            warn!(
                "data abort at ELR {:#x} has no recoverable IPA: ESR={:#x}, error={error:?}",
                context_frame.exception_pc(),
                syndrome.raw_esr(),
            );
            None
        }
    };
    let fault_virtual_address = syndrome
        .has_valid_fault_address()
        .then(|| ArmGuestVirtAddr::from_u64(FAR_EL2.get()));
    let access = decode_data_access(syndrome, |register| {
        context_frame.gpr(register.index()) as u64
    })?;

    trace!(
        "Data fault @{:?}, FAR {:?}, ELR {:#x}, ESR {:#x}, access {:?}",
        fault_ipa,
        fault_virtual_address,
        context_frame.exception_pc(),
        syndrome.raw_esr(),
        access,
    );

    Ok(ArmVmExit::DataAbort {
        abort: ArmDataAbort::new(
            fault_ipa,
            fault_virtual_address,
            context_frame.exception_pc() as u64,
            syndrome,
            access,
        ),
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

/// Handles VM-local PSCI calls made through HVC or SMC.
///
/// PSCI uses the standard-service owner ranges `0x8400_0000..=0x8400_001f`
/// and `0xc400_0000..=0xc400_001f`. Recognized but unsupported calls are
/// completed with `PSCI_RET_NOT_SUPPORTED`, so guest requests cannot fall
/// through to host firmware.
///
/// Returns `None` when the function identifier is outside the PSCI ranges.
fn handle_psci_call(ctx: &mut TrapFrame) -> Option<ArmVcpuResult<ArmVmExit>> {
    let call = crate::psci::decode(ctx.gpr[0], [ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]])?;
    Some(Ok(match call {
        crate::psci::PsciCall::Complete(result) => {
            ctx.gpr[0] = result;
            ArmVmExit::Nothing
        }
        crate::psci::PsciCall::CpuOff { state } => ArmVmExit::CpuDown { state },
        crate::psci::PsciCall::CpuOn {
            target_cpu,
            entry_point,
            context,
        } => ArmVmExit::CpuUp {
            target_cpu,
            entry_point: ArmGuestPhysAddr::from_usize(entry_point as usize),
            arg: context,
        },
        crate::psci::PsciCall::SystemOff => ArmVmExit::SystemDown,
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
            unsafe { crate::smc::smc_call(ctx.gpr[0], ctx.gpr[1], ctx.gpr[2], ctx.gpr[3]) };
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
    crate::host::handle_current_host_irq()
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
