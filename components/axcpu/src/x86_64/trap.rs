use x86::{controlregs::cr2, irq::*};
use x86_64::{registers::rflags::RFlags, structures::idt::PageFaultErrorCode};

use super::{TrapFrame, gdt};
use crate::{TrapOrigin, trap::PageFaultFlags};

/// Untrusted register image produced and consumed by trap assembly.
///
/// Kernel-origin traps do not push `rsp` or `ss`; the raw type therefore ends
/// at `rflags`. The public user register image includes those two fields, but
/// constructing a reference to that larger type here would read beyond the
/// initialized hardware frame.
#[repr(C)]
struct RawTrapFrame {
    rax: u64,
    rcx: u64,
    rdx: u64,
    rbx: u64,
    rbp: u64,
    rsi: u64,
    rdi: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
}

const _: () = {
    assert!(core::mem::size_of::<RawTrapFrame>() == core::mem::offset_of!(TrapFrame, rsp));
    assert!(
        core::mem::offset_of!(RawTrapFrame, vector) == core::mem::offset_of!(TrapFrame, vector)
    );
    assert!(core::mem::offset_of!(RawTrapFrame, rip) == core::mem::offset_of!(TrapFrame, rip));
    assert!(
        core::mem::offset_of!(RawTrapFrame, rflags) == core::mem::offset_of!(TrapFrame, rflags)
    );
};

/// Lifetime-bound view of a kernel-origin x86 trap frame.
///
/// The view deliberately provides no mutable dereference to the assembly
/// image. Probe/debug integrations can edit a copy and apply it through
/// [`Self::apply_registers`], which preserves the trap origin and vector
/// metadata.
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
    pub fn snapshot(&self) -> TrapFrame {
        TrapFrame {
            rax: self.raw.rax,
            rcx: self.raw.rcx,
            rdx: self.raw.rdx,
            rbx: self.raw.rbx,
            rbp: self.raw.rbp,
            rsi: self.raw.rsi,
            rdi: self.raw.rdi,
            r8: self.raw.r8,
            r9: self.raw.r9,
            r10: self.raw.r10,
            r11: self.raw.r11,
            r12: self.raw.r12,
            r13: self.raw.r13,
            r14: self.raw.r14,
            r15: self.raw.r15,
            vector: self.raw.vector,
            error_code: self.raw.error_code,
            rip: self.raw.rip,
            cs: self.raw.cs,
            rflags: self.raw.rflags,
            rsp: self.raw as *const RawTrapFrame as u64
                + core::mem::size_of::<RawTrapFrame>() as u64,
            ss: gdt::KDATA.0 as u64,
        }
    }

    /// Applies task-register changes while preserving trap-origin metadata.
    pub fn apply_registers(&mut self, updated: &TrapFrame) {
        self.raw.rax = updated.rax;
        self.raw.rcx = updated.rcx;
        self.raw.rdx = updated.rdx;
        self.raw.rbx = updated.rbx;
        self.raw.rbp = updated.rbp;
        self.raw.rsi = updated.rsi;
        self.raw.rdi = updated.rdi;
        self.raw.r8 = updated.r8;
        self.raw.r9 = updated.r9;
        self.raw.r10 = updated.r10;
        self.raw.r11 = updated.r11;
        self.raw.r12 = updated.r12;
        self.raw.r13 = updated.r13;
        self.raw.r14 = updated.r14;
        self.raw.r15 = updated.r15;
        self.raw.rip = updated.rip;
        self.raw.rflags = updated.rflags;
    }

    /// Returns the saved instruction pointer.
    pub const fn ip(&self) -> usize {
        self.raw.rip as usize
    }

    /// Sets the saved instruction pointer.
    pub const fn set_ip(&mut self, ip: usize) {
        self.raw.rip = ip as u64;
    }

    /// Creates the typed view at the assembly boundary.
    ///
    /// # Safety
    ///
    /// `raw` must be the uniquely borrowed, live kernel-origin frame built by
    /// the x86 trap entry and must remain valid for `'a`.
    unsafe fn from_raw(raw: &'a mut RawTrapFrame) -> Self {
        debug_assert_eq!(raw.cs & 0b11, 0);
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

core::arch::global_asm!(
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
    user_fs_base_offset = const core::mem::size_of::<TrapFrame>(),
    user_gs_base_offset = const core::mem::size_of::<TrapFrame>() + core::mem::size_of::<u64>(),
    kernel_fs_base_offset = const core::mem::size_of::<TrapFrame>() + 2 * core::mem::size_of::<u64>(),
    UDATA = const gdt::UDATA.0,
    UCODE64 = const gdt::UCODE64.0,
    SYSCALL_VECTOR = const LEGACY_SYSCALL_VECTOR,
);

pub(super) const LEGACY_SYSCALL_VECTOR: u8 = 0x80;
pub(super) const IRQ_VECTOR_START: u8 = 0x20;
pub(super) const IRQ_VECTOR_END: u8 = 0xff;

fn handle_page_fault(tf: &mut KernelTrapFrame<'_>) {
    let access_flags = err_code_to_flags(tf.raw.error_code)
        .unwrap_or_else(|e| panic!("Invalid #PF error code: {:#x}", e));
    let vaddr = va!(unsafe { cr2() });
    if crate::trap::call_page_fault_handler_with_parent_irqs(
        vaddr,
        access_flags,
        RFlags::from_bits_truncate(tf.raw.rflags).contains(RFlags::INTERRUPT_FLAG),
    ) {
        return;
    }
    #[cfg(feature = "exception-table")]
    {
        let mut updated = tf.snapshot();
        if updated.fixup_exception() {
            tf.apply_registers(&updated);
            return;
        }
    }
    let snapshot = tf.snapshot();
    let bt = snapshot.backtrace();
    panic!(
        "Unhandled #PF @ {:#x}, fault_vaddr={:#x}, error_code={:#x} ({:?}):\n{:#x?}\n{}",
        tf.raw.rip,
        vaddr,
        tf.raw.error_code,
        access_flags,
        snapshot,
        bt.kind("trap")
    );
}

fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>) {
    debug!("#BP @ {:#x} ", tf.raw.rip);
    let _ = crate::trap::breakpoint_handler(tf);
}

fn handle_debug(tf: &mut KernelTrapFrame<'_>) {
    debug!("#DB @ {:#x} ", tf.raw.rip);
    if crate::trap::debug_handler(tf) {
        return;
    }
    // Kernel-mode #DB was not claimed by any handler.
    // Unclaimed user-mode #DB is routed through the user-space exception loop
    // (.Ltrap_user → .Lexit_user in trap.S), so `x86_trap_handler` is only
    // reached for kernel-mode traps. An unhandled kernel #DB is a fatal
    // condition: if resumed the CPU re-executes the faulting instruction,
    // likely looping into a triple fault.
    warn!("Unhandled kernel #DB @ {:#x}", tf.raw.rip);
    let snapshot = tf.snapshot();
    let bt = snapshot.backtrace();
    panic!(
        "Unhandled #DB @ {:#x}, error_code={:#x}:\n{:#x?}\n{}",
        tf.raw.rip,
        tf.raw.error_code,
        snapshot,
        bt.kind("trap")
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn x86_trap_handler(raw: *mut RawTrapFrame) {
    // SAFETY: every x86 trap vector allocates one complete, exclusively owned
    // frame and passes its aligned stack address in the C argument register.
    let raw = unsafe { &mut *raw };
    let mut tf = unsafe { KernelTrapFrame::from_raw(raw) };
    match tf.raw.vector as u8 {
        PAGE_FAULT_VECTOR => handle_page_fault(&mut tf),
        BREAKPOINT_VECTOR => handle_breakpoint(&mut tf),
        DEBUG_VECTOR => handle_debug(&mut tf),
        GENERAL_PROTECTION_FAULT_VECTOR => {
            let snapshot = tf.snapshot();
            let bt = snapshot.backtrace();
            panic!(
                "#GP @ {:#x}, error_code={:#x}:\n{:#x?}\n{}",
                tf.raw.rip,
                tf.raw.error_code,
                snapshot,
                bt.kind("trap")
            );
        }
        IRQ_VECTOR_START..=IRQ_VECTOR_END => {
            // SAFETY: this branch is reached only from the x86 IDT IRQ entry;
            // the trap frame retains ownership of the interrupted RFLAGS.
            unsafe { crate::trap::dispatch_arch_irq(tf.raw.vector as _) };
        }
        _ => {
            let snapshot = tf.snapshot();
            let bt = snapshot.backtrace();
            panic!(
                "Unhandled exception {} ({}, error_code={:#x}) @ {:#x}:\n{:#x?}\n{}",
                tf.raw.vector,
                vec_to_str(tf.raw.vector),
                tf.raw.error_code,
                tf.raw.rip,
                snapshot,
                bt.kind("trap")
            );
        }
    }
}

fn vec_to_str(vec: u64) -> &'static str {
    if vec < 32 {
        EXCEPTIONS[vec as usize].mnemonic
    } else {
        "Unknown"
    }
}

pub(super) fn err_code_to_flags(err_code: u64) -> Result<PageFaultFlags, u64> {
    let code = PageFaultErrorCode::from_bits_truncate(err_code);
    let reserved_bits = (PageFaultErrorCode::CAUSED_BY_WRITE
        | PageFaultErrorCode::USER_MODE
        | PageFaultErrorCode::INSTRUCTION_FETCH
        | PageFaultErrorCode::PROTECTION_VIOLATION)
        .complement();
    if code.intersects(reserved_bits) {
        Err(err_code)
    } else {
        let mut flags = PageFaultFlags::empty();
        if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            flags |= PageFaultFlags::WRITE;
        } else {
            flags |= PageFaultFlags::READ;
        }
        if code.contains(PageFaultErrorCode::USER_MODE) {
            flags |= PageFaultFlags::USER;
        }
        if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
            flags |= PageFaultFlags::EXECUTE;
        }
        Ok(flags)
    }
}
