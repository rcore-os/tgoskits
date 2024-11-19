use aarch64_cpu::registers::{Readable, ESR_EL2, HCR_EL2, SCTLR_EL1, VTCR_EL2, VTTBR_EL2};

use axaddrspace::GuestPhysAddr;
use axerrno::{AxError, AxResult};
use axvcpu::{AccessWidth, AxVCpuExitReason};

use crate::exception_utils::{
    exception_class, exception_class_value, exception_data_abort_access_is_write,
    exception_data_abort_access_reg, exception_data_abort_access_reg_width,
    exception_data_abort_access_width, exception_data_abort_handleable,
    exception_data_abort_is_permission_fault, exception_data_abort_is_translate_fault,
    exception_esr, exception_fault_addr, exception_next_instruction_step,
};
use crate::TrapFrame;

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
/// * `ctx` - A mutable reference to the `TrapFrame`, which contains the saved state of the
///           guest VM's CPU registers at the time of the exception.
///
/// # Returns
///
/// An `AxResult` containing an `AxVCpuExitReason` indicating the reason for the VM exit.
/// This could be due to a hypervisor call (`Hypercall`) or other reasons such as data aborts.
///
/// # Panics
///
/// If an unhandled exception class is encountered, the function will panic, outputting
/// details about the exception including the instruction pointer, faulting address, exception
/// syndrome register (ESR), and system control registers.
///
pub fn handle_exception_sync(ctx: &mut TrapFrame) -> AxResult<AxVCpuExitReason> {
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
            Ok(AxVCpuExitReason::Hypercall {
                nr: ctx.gpr[0],
                args: [
                    ctx.gpr[1], ctx.gpr[2], ctx.gpr[3], ctx.gpr[4], ctx.gpr[5], ctx.gpr[6],
                ],
            })
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

fn handle_data_abort(context_frame: &mut TrapFrame) -> AxResult<AxVCpuExitReason> {
    let addr = exception_fault_addr()?;
    debug!("data fault addr {:?}, esr: 0x{:x}", addr, exception_esr());

    let access_width = exception_data_abort_access_width();
    let is_write = exception_data_abort_access_is_write();
    //let sign_ext = exception_data_abort_access_is_sign_ext();
    let reg = exception_data_abort_access_reg();
    let reg_width = exception_data_abort_access_reg_width();

    let elr = context_frame.exception_pc();
    let val = elr + exception_next_instruction_step();
    context_frame.set_exception_pc(val);

    let width = match AccessWidth::try_from(access_width) {
        Ok(access_width) => access_width,
        Err(_) => return Err(AxError::InvalidInput),
    };

    let reg_width = match AccessWidth::try_from(reg_width) {
        Ok(reg_width) => reg_width,
        Err(_) => return Err(AxError::InvalidInput),
    };

    if !exception_data_abort_handleable() {
        panic!(
            "Core data abort not handleable {:#x}, esr {:#x}",
            addr,
            exception_esr()
        );
    }

    if !exception_data_abort_is_translate_fault() {
        if exception_data_abort_is_permission_fault() {
            return Err(AxError::Unsupported);
        } else {
            panic!("Core data abort is not translate fault {:#x}", addr,);
        }
    }

    if is_write {
        return Ok(AxVCpuExitReason::MmioWrite {
            addr,
            width,
            data: context_frame.gpr(reg) as u64,
        });
    }
    Ok(AxVCpuExitReason::MmioRead {
        addr,
        width,
        reg,
        reg_width,
    })
}

/// Handles HVC or SMC exceptions that serve as psci (Power State Coordination Interface) calls.
///
/// A hvc or smc call with the function in range 0x8000_0000..=0x8000_001F  (when the 32-bit
/// hvc/smc calling convention is used) or 0xC000_0000..=0xC000_001F (when the 64-bit hvc/smc
/// calling convention is used) is a psci call. This function handles them all.
///
/// Returns `None` if the HVC is not a psci call.
fn handle_psci_call(ctx: &mut TrapFrame) -> Option<AxResult<AxVCpuExitReason>> {
    const PSCI_FN_RANGE_32: core::ops::RangeInclusive<u64> = 0x8400_0000..=0x8400_001F;
    const PSCI_FN_RANGE_64: core::ops::RangeInclusive<u64> = 0xC400_0000..=0xC400_001F;

    const _PSCI_FN_CPU_SUSPEND: u64 = 0x1;
    const PSCI_FN_CPU_OFF: u64 = 0x2;
    const PSCI_FN_CPU_ON: u64 = 0x3;
    const _PSCI_FN_MIGRATE: u64 = 0x5;
    const PSCI_FN_SYSTEM_OFF: u64 = 0x8;
    const _PSCI_FN_SYSTEM_RESET: u64 = 0x9;

    let fn_ = ctx.gpr[0];
    let fn_offset = if PSCI_FN_RANGE_32.contains(&fn_) {
        Some(fn_ - PSCI_FN_RANGE_32.start())
    } else if PSCI_FN_RANGE_64.contains(&fn_) {
        Some(fn_ - PSCI_FN_RANGE_64.start())
    } else {
        None
    };

    fn_offset.map(|fn_offset| match fn_offset {
        PSCI_FN_CPU_OFF => Ok(AxVCpuExitReason::CpuDown { _state: ctx.gpr[1] }),
        PSCI_FN_CPU_ON => Ok(AxVCpuExitReason::CpuUp {
            target_cpu: ctx.gpr[1],
            entry_point: GuestPhysAddr::from(ctx.gpr[2] as usize),
            arg: ctx.gpr[3],
        }),
        PSCI_FN_SYSTEM_OFF => Ok(AxVCpuExitReason::SystemDown),
        _ => Err(AxError::Unsupported),
    })
}

/// Dispatches IRQs to the appropriate handler provided by the underlying host OS,
/// which is registered at [`crate::pcpu::IRQ_HANDLER`] during `Aarch64PerCpu::new()`.
fn dispatch_irq() {
    unsafe { crate::pcpu::IRQ_HANDLER.current_ref_raw() }
        .get()
        .unwrap()()
}

/// A trampoline function for handling exceptions (VM exits) in EL2.
///
/// Functionality:
///
/// 1. **Check if VCPU is running:**
///    - The `vcpu_running` function is called to check if the VCPU is currently running.
///     If the VCPU is running, the control flow is transferred to the `return_run_guest` function.
///
/// 2. **Dispatch IRQ:**
///   - If there is no active vcpu running, the `dispatch_irq` function is called to handle the IRQ,
///     which will dispatch this irq routine to the underlining host OS.
///   - The IRQ handling routine will end up calling `exception_return_el2` here.
///
/// Note that the `return_run_guest` will never return.
#[naked]
#[no_mangle]
unsafe extern "C" fn vmexit_trampoline() {
    core::arch::asm!(
        "bl {vcpu_running}", // Check if vcpu is running.
        // If vcpu_running returns true, jump to `return_run_guest`,
        // after that the control flow is handed back to Aarch64VCpu.run(),
        // simulating the normal return of the `run_guest` function.
        "cbnz x0, {return_run_guest}",
        // If vcpu_running returns false, there is no active vcpu running,
        // jump to `dispatch_irq`.
        "bl {dispatch_irq}",
        // Return from exception.
        "b  .Lexception_return_el2",
        vcpu_running = sym crate::vcpu::vcpu_running,
        return_run_guest = sym return_run_guest,
        dispatch_irq = sym dispatch_irq,
        options(noreturn),
    )
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
///       (adding `34 * 8` to the stack pointer) according to the memory layout of `Aarch64VCpu` struct,
///       which makes current `sp` point to the address of `host_stack_top`.
///       The host stack top value is restored by `ldr`.
///
/// 2. **Restore Host Context:**
///     - The `restore_regs_from_stack!()` macro is invoked to restore the host function context
///       from the stack. This macro handles the restoration of the host's callee-saved general-purpose
///       registers (`x19` to `x30`).
///
/// 3. **Restore Host Control Flow:**
///     - The `ret` instruction is used to return control to the host context after
///       the guest context has been saved in `Aarch64VCpu` struct and the host context restored.
///       Finally the control flow is returned back to `Aarch64VCpu.run()` in [vcpu.rs].
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
#[naked]
#[no_mangle]
unsafe extern "C" fn return_run_guest() -> ! {
    core::arch::asm!(
        // Curretly `sp` points to the base address of `Aarch64VCpu.ctx`, which stores guest's `TrapFrame`.
        "add x9, sp, 34 * 8", // Skip the exception frame.
        // Currently `x9` points to `&Aarch64VCpu.host_stack_top`, see `run_guest()` in vcpu.rs.
        "ldr x10, [x9]", // Get `host_stack_top` value from `&Aarch64VCpu.host_stack_top`.
        "mov sp, x10",   // Set `sp` as the host stack top.
        restore_regs_from_stack!(), // Restore host function context frame.
        "ret", // Control flow is handed back to Aarch64VCpu.run(), simulating the normal return of the `run_guest` function.
        options(noreturn),
    )
}

/// Deal with invalid aarch64 exception.
#[no_mangle]
fn invalid_exception_el2(tf: &mut TrapFrame, kind: TrapKind, source: TrapSource) {
    panic!(
        "Invalid exception {:?} from {:?}:\n{:#x?}",
        kind, source, tf
    );
}
