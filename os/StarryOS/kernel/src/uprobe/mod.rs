//! Per-process uprobe support.
//!
//! A uprobe plants an `int3` (or arch breakpoint) into a *user* process' text
//! and runs an eBPF program on every hit. Unlike kprobes — which share one
//! global manager because the kernel text is shared — each process owns its own
//! [`KprobeManager`](crate::kprobe::KprobeManager) /
//! [`KprobePointList`](crate::kprobe::KprobePointList) on
//! [`ProcessData`](crate::task::ProcessData), since user addresses are only
//! meaningful within one address space.
//!
//! The heavy lifting (instruction decode, out-of-line single-step) lives in the
//! `kprobe` crate; this module just routes the per-process manager and the
//! trap/debug frames into it. The user-space breakpoint insertion and the
//! out-of-line exec page come from the `KprobeAuxiliaryOps` user-mode paths in
//! [`crate::kprobe`].

use alloc::sync::Arc;

use ax_task::current;
use kprobe::{ProbeBuilder, Uprobe};

use crate::{
    kprobe::{KernelKprobeOps, KernelRawMutex, ptregs_write_back, trapframe_to_ptregs},
    task::AsThread,
};

/// Concrete `kprobe::Uprobe` parameterized on the kernel's raw mutex and
/// auxiliary ops (the same `L` / `F` the kprobe types use).
pub type KernelUprobe = Uprobe<KernelRawMutex, KernelKprobeOps>;

/// Register a uprobe into the *current* process' per-process manager.
pub fn register_uprobe(builder: ProbeBuilder<KernelKprobeOps>) -> Arc<KernelUprobe> {
    let curr = current();
    let thread = curr.as_thread();
    let mut manager = thread.proc_data.uprobe_manager.lock();
    let mut point_list = thread.proc_data.uprobe_point_list.lock();
    kprobe::register_uprobe(&mut manager, &mut point_list, builder)
}

/// Unregister a previously registered uprobe from the current process.
pub fn unregister_uprobe(uprobe: Arc<KernelUprobe>) {
    let curr = current();
    let thread = curr.as_thread();
    let mut manager = thread.proc_data.uprobe_manager.lock();
    let mut point_list = thread.proc_data.uprobe_point_list.lock();
    kprobe::unregister_uprobe(&mut manager, &mut point_list, uprobe);
}

/// Dispatch a breakpoint exception to the current process' uprobe manager.
/// Returns `Some(())` if a uprobe handled it.
///
/// Runs from exception context (IRQs disabled), so the per-process manager — a
/// sleeping mutex (see [`crate::task::ProcessData::uprobe_manager`]) — is taken
/// with `try_lock()`, which is a single CAS and safe here. At fire time the
/// manager is uncontended (arming happens in syscall context on the same task),
/// so the lock is always acquired; a contended miss just reports "unhandled".
pub fn break_uprobe_handler(tf: &mut ax_runtime::hal::cpu::TrapFrame) -> Option<()> {
    let curr = current();
    let mut manager = curr.as_thread().proc_data.uprobe_manager.try_lock()?;
    let mut pt_regs = trapframe_to_ptregs(tf);
    let res = kprobe::uprobe_handler_from_break(&mut manager, &mut pt_regs);
    ptregs_write_back(&pt_regs, tf);
    res
}

/// Dispatch a debug (single-step) exception to the current process' uprobe
/// manager — the out-of-line step completion path on x86_64.
#[cfg(target_arch = "x86_64")]
pub fn debug_uprobe_handler(tf: &mut ax_runtime::hal::cpu::TrapFrame) -> Option<()> {
    let curr = current();
    // `try_lock()` for the same reason as `break_uprobe_handler`: exception
    // context, sleeping mutex.
    let mut manager = curr.as_thread().proc_data.uprobe_manager.try_lock()?;
    let mut pt_regs = trapframe_to_ptregs(tf);
    let res = kprobe::uprobe_handler_from_debug(&mut manager, &mut pt_regs);
    ptregs_write_back(&pt_regs, tf);
    res
}
