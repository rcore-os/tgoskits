use core::mem::size_of;

#[cfg(feature = "tls")]
use ax_cpu_local::{CPU_AREA_ENTRY_SCRATCH0_OFFSET, CPU_AREA_ENTRY_SCRATCH1_OFFSET};
use ax_cpu_local::{CPU_AREA_KERNEL_STACK_POINTER_OFFSET, CPU_AREA_USER_TRAP_FRAME_OFFSET};
#[cfg(not(feature = "tls"))]
use ax_cpu_local::{
    CURRENT_THREAD_CPU_BASE_OFFSET, CURRENT_THREAD_TRAP_SCRATCH0_OFFSET,
    CURRENT_THREAD_TRAP_SCRATCH1_OFFSET,
};
#[cfg(feature = "fp-simd")]
use riscv::register::sstatus;
use riscv::{
    interrupt::{
        Trap,
        supervisor::{Exception as E, Interrupt as I},
    },
    register::{scause, stval},
};

use super::TrapFrame;
use crate::{TrapOrigin, trap::PageFaultFlags};

/// Untrusted register image produced and consumed by trap assembly.
#[repr(transparent)]
struct RawTrapFrame(TrapFrame);

const _: () = {
    assert!(size_of::<RawTrapFrame>() == size_of::<TrapFrame>());
    assert!(core::mem::align_of::<RawTrapFrame>() == core::mem::align_of::<TrapFrame>());
};

/// Lifetime-bound view of a supervisor-origin RISC-V trap frame.
///
/// CPU-owned anchor state is absent from the exposed register image. In
/// LinuxCurrent, kernel `sscratch` is canonical zero and `tp` identifies the
/// current header; UnikernelTls keeps the CPU prefix in `sscratch`. The saved
/// status word remains owned by trap return and survives probe writeback.
pub struct KernelTrapFrame<'a> {
    raw: &'a mut RawTrapFrame,
    _not_send: core::marker::PhantomData<*mut ()>,
}

impl<'a> KernelTrapFrame<'a> {
    /// Returns the privilege domain represented by this view.
    pub const fn origin(&self) -> TrapOrigin {
        TrapOrigin::Kernel
    }

    /// Copies the saved register image for inspection or probe emulation.
    pub const fn snapshot(&self) -> TrapFrame {
        self.raw.0
    }

    /// Applies task-register changes while preserving trap-return privilege.
    pub fn apply_registers(&mut self, updated: &TrapFrame) {
        let spp = self.raw.0.sstatus.spp();
        let kernel_gp = self.raw.0.regs.gp;
        let kernel_tp = self.raw.0.regs.tp;
        self.raw.0 = *updated;
        self.raw.0.sstatus.set_spp(spp);
        self.raw.0.regs.gp = kernel_gp;
        self.raw.0.regs.tp = kernel_tp;
    }

    /// Returns the saved instruction pointer.
    pub const fn ip(&self) -> usize {
        self.raw.0.ip()
    }

    /// Sets the saved instruction pointer.
    pub const fn set_ip(&mut self, ip: usize) {
        self.raw.0.set_ip(ip);
    }

    /// Creates the typed view at the assembly boundary.
    ///
    /// # Safety
    ///
    /// `raw` must be the uniquely borrowed, live supervisor-origin frame built
    /// by `trap_vector_base` and must remain valid for `'a`.
    unsafe fn from_raw(raw: &'a mut RawTrapFrame) -> Self {
        debug_assert_eq!(raw.0.origin(), TrapOrigin::Kernel);
        Self {
            raw,
            _not_send: core::marker::PhantomData,
        }
    }
}

impl core::fmt::Debug for KernelTrapFrame<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.snapshot().fmt(formatter)
    }
}

#[cfg(not(feature = "tls"))]
core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap.S"),
    trapframe_size = const size_of::<RawTrapFrame>(),
    kernel_stack_pointer_index = const CPU_AREA_KERNEL_STACK_POINTER_OFFSET / size_of::<usize>(),
    user_trap_frame_index = const CPU_AREA_USER_TRAP_FRAME_OFFSET / size_of::<usize>(),
    thread_cpu_base_index = const CURRENT_THREAD_CPU_BASE_OFFSET / size_of::<usize>(),
    thread_scratch0_index = const CURRENT_THREAD_TRAP_SCRATCH0_OFFSET / size_of::<usize>(),
    thread_scratch1_index = const CURRENT_THREAD_TRAP_SCRATCH1_OFFSET / size_of::<usize>(),
);

#[cfg(feature = "tls")]
core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap_tls.S"),
    trapframe_size = const size_of::<RawTrapFrame>(),
    kernel_stack_pointer_index = const CPU_AREA_KERNEL_STACK_POINTER_OFFSET / size_of::<usize>(),
    user_trap_frame_index = const CPU_AREA_USER_TRAP_FRAME_OFFSET / size_of::<usize>(),
    entry_scratch0_index = const CPU_AREA_ENTRY_SCRATCH0_OFFSET / size_of::<usize>(),
    entry_scratch1_index = const CPU_AREA_ENTRY_SCRATCH1_OFFSET / size_of::<usize>(),
);

fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>) {
    debug!("Exception(Breakpoint) @ {:#x} ", tf.raw.0.sepc);
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.set_ip(tf.ip() + 2);
}

fn handle_page_fault(tf: &mut KernelTrapFrame<'_>, access_flags: PageFaultFlags) {
    let vaddr = va!(stval::read());
    if crate::trap::call_page_fault_handler_with_parent_irqs(
        vaddr,
        access_flags,
        tf.raw.0.sstatus.spie(),
    ) {
        return;
    }
    #[cfg(feature = "exception-table")]
    if tf.raw.0.fixup_exception() {
        return;
    }
    let snapshot = tf.snapshot();
    let bt = snapshot.backtrace();
    panic!(
        "Unhandled Supervisor Page Fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}\n{}",
        tf.raw.0.sepc,
        vaddr,
        access_flags,
        snapshot,
        bt.kind("trap")
    );
}

/// Raw assembly-to-Rust trap entry.
///
/// # Safety
///
/// `raw_tf` must point to the uniquely borrowed, fully initialized trap frame
/// built by `trap_vector_base`. The frame must remain valid until this function
/// returns to the assembly restore path.
#[unsafe(no_mangle)]
unsafe extern "C" fn riscv_trap_handler(raw_tf: *mut RawTrapFrame) {
    // SAFETY: the caller contract is exactly the trap assembly's frame
    // construction invariant, and this is its only Rust borrow.
    let raw = unsafe { &mut *raw_tf };
    let mut tf = unsafe { KernelTrapFrame::from_raw(raw) };
    handle_trap(&mut tf);
}

fn handle_trap(tf: &mut KernelTrapFrame<'_>) {
    let scause = scause::read();
    if let Ok(cause) = scause.cause().try_into::<I, E>() {
        match cause {
            Trap::Exception(E::LoadPageFault) => handle_page_fault(tf, PageFaultFlags::READ),
            Trap::Exception(E::StorePageFault) => handle_page_fault(tf, PageFaultFlags::WRITE),
            Trap::Exception(E::InstructionPageFault) => {
                handle_page_fault(tf, PageFaultFlags::EXECUTE)
            }
            Trap::Exception(E::Breakpoint) => handle_breakpoint(tf),
            Trap::Interrupt(_) => {
                // SAFETY: the RISC-V trap entry owns the saved sstatus and
                // returns through the matching kernel exception frame.
                unsafe { crate::trap::dispatch_arch_irq(scause.bits()) };
            }
            _ => {
                let snapshot = tf.snapshot();
                let bt = snapshot.backtrace();
                panic!(
                    "Unhandled trap {:?} @ {:#x}, stval={:#x}:\n{:#x?}\n{}",
                    cause,
                    tf.raw.0.sepc,
                    stval::read(),
                    snapshot,
                    bt.kind("trap")
                );
            }
        }
    } else {
        let snapshot = tf.snapshot();
        let bt = snapshot.backtrace();
        panic!(
            "Unknown trap {:#x?} @ {:#x}:\n{:#x?}\n{}",
            scause.cause(),
            tf.raw.0.sepc,
            snapshot,
            bt.kind("trap")
        );
    }

    // Update tf.sstatus to preserve current hardware FS state
    // This replaces the assembly-level FS handling workaround
    #[cfg(feature = "fp-simd")]
    tf.raw.0.sstatus.set_fs(sstatus::read().fs());
}
