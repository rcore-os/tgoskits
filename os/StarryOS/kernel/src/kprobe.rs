//! Kernel probe (kprobe) subsystem for StarryOS.
//!
//! This module provides dynamic tracing support by allowing breakpoint
//! insertion at kernel function entry/return points. It integrates the
//! [`kprobe`] crate with StarryOS kernel infrastructure.
//!
//! # Architecture Support
//!
//! All four supported architectures are enabled: x86_64, riscv64, aarch64,
//! and loongarch64. Each architecture provides TrapFrame↔PtRegs register
//! conversion to bridge the kernel's trap frame format with the kprobe
//! crate's portable `PtRegs` type.
//!
//! # Feature Gate
//!
//! The kprobe module is gated behind the `kprobe` feature (enabled by default).
//! When disabled, breakpoint and debug exception handlers fall back to stubs
//! that return `false`.
//!
//! # Key Components
//!
//! - [`KernelRawMutex`]: CAS-based spinlock implementing `lock_api::RawMutex`
//! - [`KernelKprobeOps`]: Platform-specific auxiliary operations for the kprobe crate
//! - [`handle_breakpoint`]: Entry point for breakpoint exceptions (INT3/EBREAK/BRK)
//! - [`handle_debug`]: Entry point for debug exceptions (x86_64 single-step only)

use alloc::alloc::{Layout, alloc, dealloc};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use kprobe::KprobeAuxiliaryOps;
use lock_api::RawMutex;

use crate::task::AsThread;

/// A CAS-based spinlock implementing [`RawMutex`] for use with [`kprobe::ProbeManager`].
///
/// Uses `compare_exchange_weak` in a busy-wait loop, suitable for interrupt
/// context where sleeping is not allowed.
pub struct KernelRawMutex {
    locked: AtomicBool,
}

unsafe impl RawMutex for KernelRawMutex {
    const INIT: Self = KernelRawMutex {
        locked: AtomicBool::new(false),
    };

    type GuardMarker = lock_api::GuardNoSend;

    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn try_lock(&self) -> bool {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    unsafe fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

/// StarryOS-specific implementation of [`KprobeAuxiliaryOps`].
///
/// Provides the platform glue that the `kprobe` crate needs: memory copy,
/// page table manipulation for breakpoint insertion, executable memory
/// allocation, and per-task kretprobe instance management.
#[derive(Debug)]
pub struct KernelKprobeOps;

impl KprobeAuxiliaryOps for KernelKprobeOps {
    fn copy_memory(src: *const u8, dst: *mut u8, len: usize, user_pid: Option<i32>) {
        if let Some(_pid) = user_pid {
            unsafe {
                let buf =
                    core::slice::from_raw_parts_mut(dst as *mut core::mem::MaybeUninit<u8>, len);
                if let Err(e) = starry_vm::vm_read_slice(src, buf) {
                    warn!("kprobe copy_memory: vm_read_slice failed: {:?}", e);
                }
            }
        } else {
            unsafe {
                core::ptr::copy_nonoverlapping(src, dst, len);
            }
        }
    }

    fn set_writeable_for_address<F: FnOnce(*mut u8)>(
        address: usize,
        len: usize,
        user_pid: Option<i32>,
        action: F,
    ) {
        if user_pid.is_some() {
            unimplemented!("user space breakpoint insertion not yet supported")
        }
        let addr = VirtAddr::from(address);
        let aligned_addr = addr.align_down_4k();
        let aligned_end = (addr + len).align_up_4k();
        let aligned_length: usize = aligned_end - aligned_addr;

        crate::stop_machine::stop_machine(
            move || {
                let mut guard = ax_mm::kernel_aspace().lock();
                let (_, original_flags, _) = guard
                    .page_table()
                    .query(aligned_addr)
                    .expect("kprobe: set_writeable: address not mapped");
                guard
                    .protect(
                        aligned_addr,
                        aligned_length,
                        original_flags | ax_hal::paging::MappingFlags::WRITE,
                    )
                    .expect("kprobe: set_writeable: protect failed");
                flush_tlb_range(aligned_addr, aligned_length);
                action(addr.as_mut_ptr());
                #[cfg(target_arch = "aarch64")]
                ax_hal::asm::clean_dcache_range_to_pou(addr, len);
                guard
                    .protect(aligned_addr, aligned_length, original_flags)
                    .expect("kprobe: set_writeable: restore failed");
            },
            move || {
                flush_tlb_range(aligned_addr, aligned_length);
                ax_hal::asm::flush_icache_all();
            },
        );
    }

    fn alloc_kernel_exec_memory() -> *mut u8 {
        let layout = Layout::from_size_align(PAGE_SIZE_4K, PAGE_SIZE_4K).unwrap();
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            panic!("kprobe: alloc_kernel_exec_memory failed: OOM");
        }
        ptr
    }

    fn free_kernel_exec_memory(ptr: *mut u8) {
        let layout = Layout::from_size_align(PAGE_SIZE_4K, PAGE_SIZE_4K).unwrap();
        unsafe { dealloc(ptr, layout) }
    }

    fn alloc_user_exec_memory<F: FnOnce(*mut u8)>(_pid: Option<i32>, _action: F) -> *mut u8 {
        unimplemented!("user exec memory allocation for uprobes not yet supported")
    }

    fn free_user_exec_memory(_pid: Option<i32>, _ptr: *mut u8) {
        unimplemented!("user exec memory deallocation for uprobes not yet supported")
    }

    fn insert_kretprobe_instance_to_task(instance: kprobe::retprobe::RetprobeInstance) {
        let curr = ax_task::current();
        curr.as_thread().kretprobe_stack.lock().push(instance);
    }

    fn pop_kretprobe_instance_from_task() -> kprobe::retprobe::RetprobeInstance {
        let curr = ax_task::current();
        curr.as_thread()
            .kretprobe_stack
            .lock()
            .pop()
            .expect("kretprobe instance stack underflow")
    }
}

type KprobeManager = kprobe::ProbeManager<KernelRawMutex, KernelKprobeOps>;

static KPROBE_MANAGER: ax_sync::spin::SpinNoIrq<Option<KprobeManager>> =
    ax_sync::spin::SpinNoIrq::new(None);

fn with_manager<F, R>(f: F) -> R
where
    F: FnOnce(&mut KprobeManager) -> R,
{
    let mut guard = KPROBE_MANAGER.lock();
    if guard.is_none() {
        *guard = Some(KprobeManager::default());
    }
    f(guard.as_mut().expect("kprobe: manager not initialized"))
}

/// Convert the kernel's [`TrapFrame`](ax_hal::context::TrapFrame) to kprobe's
/// portable [`PtRegs`](kprobe::PtRegs) format.
///
/// Each architecture maps its trap frame registers to the corresponding
/// `PtRegs` fields. CSRs or status registers that are not available in
/// `TrapFrame` are set to zero.
fn trapframe_to_ptregs(tf: &ax_hal::context::TrapFrame) -> kprobe::PtRegs {
    #[cfg(target_arch = "x86_64")]
    {
        kprobe::PtRegs {
            r15: tf.r15 as usize,
            r14: tf.r14 as usize,
            r13: tf.r13 as usize,
            r12: tf.r12 as usize,
            rbp: tf.rbp as usize,
            rbx: tf.rbx as usize,
            r11: tf.r11 as usize,
            r10: tf.r10 as usize,
            r9: tf.r9 as usize,
            r8: tf.r8 as usize,
            rax: tf.rax as usize,
            rcx: tf.rcx as usize,
            rdx: tf.rdx as usize,
            rsi: tf.rsi as usize,
            rdi: tf.rdi as usize,
            orig_rax: 0,
            rip: tf.rip as usize,
            cs: tf.cs as usize,
            rflags: tf.rflags as usize,
            rsp: tf.rsp as usize,
            ss: tf.ss as usize,
        }
    }
    #[cfg(target_arch = "riscv64")]
    {
        kprobe::PtRegs {
            epc: tf.sepc,
            ra: tf.regs.ra,
            sp: tf.regs.sp,
            gp: tf.regs.gp,
            tp: tf.regs.tp,
            t0: tf.regs.t0,
            t1: tf.regs.t1,
            t2: tf.regs.t2,
            s0: tf.regs.s0,
            s1: tf.regs.s1,
            a0: tf.regs.a0,
            a1: tf.regs.a1,
            a2: tf.regs.a2,
            a3: tf.regs.a3,
            a4: tf.regs.a4,
            a5: tf.regs.a5,
            a6: tf.regs.a6,
            a7: tf.regs.a7,
            s2: tf.regs.s2,
            s3: tf.regs.s3,
            s4: tf.regs.s4,
            s5: tf.regs.s5,
            s6: tf.regs.s6,
            s7: tf.regs.s7,
            s8: tf.regs.s8,
            s9: tf.regs.s9,
            s10: tf.regs.s10,
            s11: tf.regs.s11,
            t3: tf.regs.t3,
            t4: tf.regs.t4,
            t5: tf.regs.t5,
            t6: tf.regs.t6,
            status: tf.sstatus.bits(),
            badaddr: 0,
            cause: 0,
            orig_a0: tf.regs.a0,
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        kprobe::PtRegs {
            regs: tf.x,
            sp: 0,
            pc: tf.elr,
            pstate: tf.spsr,
            orig_x0: tf.x[0],
            syscallno: -1,
            unused2: 0,
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        kprobe::PtRegs {
            regs: [
                tf.regs.zero,
                tf.regs.ra,
                tf.regs.tp,
                tf.regs.sp,
                tf.regs.a0,
                tf.regs.a1,
                tf.regs.a2,
                tf.regs.a3,
                tf.regs.a4,
                tf.regs.a5,
                tf.regs.a6,
                tf.regs.a7,
                tf.regs.t0,
                tf.regs.t1,
                tf.regs.t2,
                tf.regs.t3,
                tf.regs.t4,
                tf.regs.t5,
                tf.regs.t6,
                tf.regs.t7,
                tf.regs.t8,
                tf.regs.u0,
                tf.regs.fp,
                tf.regs.s0,
                tf.regs.s1,
                tf.regs.s2,
                tf.regs.s3,
                tf.regs.s4,
                tf.regs.s5,
                tf.regs.s6,
                tf.regs.s7,
                tf.regs.s8,
            ],
            orig_a0: tf.regs.a0,
            csr_era: tf.era,
            csr_badvaddr: 0,
            csr_crmd: 0,
            csr_prmd: tf.prmd,
            csr_euen: 0,
            csr_ecfg: 0,
            csr_estat: 0,
        }
    }
}

/// Write back modified [`PtRegs`](kprobe::PtRegs) values to the kernel's
/// [`TrapFrame`](ax_hal::context::TrapFrame).
///
/// This is called after a kprobe handler returns `Some(())` to apply any
/// register modifications the handler made (e.g., altering return values
/// or redirecting control flow).
fn ptregs_write_back(pt: &kprobe::PtRegs, tf: &mut ax_hal::context::TrapFrame) {
    #[cfg(target_arch = "x86_64")]
    {
        tf.r15 = pt.r15 as u64;
        tf.r14 = pt.r14 as u64;
        tf.r13 = pt.r13 as u64;
        tf.r12 = pt.r12 as u64;
        tf.rbp = pt.rbp as u64;
        tf.rbx = pt.rbx as u64;
        tf.r11 = pt.r11 as u64;
        tf.r10 = pt.r10 as u64;
        tf.r9 = pt.r9 as u64;
        tf.r8 = pt.r8 as u64;
        tf.rax = pt.rax as u64;
        tf.rcx = pt.rcx as u64;
        tf.rdx = pt.rdx as u64;
        tf.rsi = pt.rsi as u64;
        tf.rdi = pt.rdi as u64;
        tf.rip = pt.rip as u64;
        tf.cs = pt.cs as u64;
        tf.rflags = pt.rflags as u64;
        tf.rsp = pt.rsp as u64;
        tf.ss = pt.ss as u64;
    }
    #[cfg(target_arch = "riscv64")]
    {
        tf.sepc = pt.epc;
        tf.regs.ra = pt.ra;
        tf.regs.sp = pt.sp;
        tf.regs.gp = pt.gp;
        tf.regs.tp = pt.tp;
        tf.regs.t0 = pt.t0;
        tf.regs.t1 = pt.t1;
        tf.regs.t2 = pt.t2;
        tf.regs.s0 = pt.s0;
        tf.regs.s1 = pt.s1;
        tf.regs.a0 = pt.a0;
        tf.regs.a1 = pt.a1;
        tf.regs.a2 = pt.a2;
        tf.regs.a3 = pt.a3;
        tf.regs.a4 = pt.a4;
        tf.regs.a5 = pt.a5;
        tf.regs.a6 = pt.a6;
        tf.regs.a7 = pt.a7;
        tf.regs.s2 = pt.s2;
        tf.regs.s3 = pt.s3;
        tf.regs.s4 = pt.s4;
        tf.regs.s5 = pt.s5;
        tf.regs.s6 = pt.s6;
        tf.regs.s7 = pt.s7;
        tf.regs.s8 = pt.s8;
        tf.regs.s9 = pt.s9;
        tf.regs.s10 = pt.s10;
        tf.regs.s11 = pt.s11;
        tf.regs.t3 = pt.t3;
        tf.regs.t4 = pt.t4;
        tf.regs.t5 = pt.t5;
        tf.regs.t6 = pt.t6;
    }
    #[cfg(target_arch = "aarch64")]
    {
        tf.x = pt.regs;
        tf.elr = pt.pc;
        tf.spsr = pt.pstate;
    }
    #[cfg(target_arch = "loongarch64")]
    {
        tf.regs.zero = pt.regs[0];
        tf.regs.ra = pt.regs[1];
        tf.regs.tp = pt.regs[2];
        tf.regs.sp = pt.regs[3];
        tf.regs.a0 = pt.regs[4];
        tf.regs.a1 = pt.regs[5];
        tf.regs.a2 = pt.regs[6];
        tf.regs.a3 = pt.regs[7];
        tf.regs.a4 = pt.regs[8];
        tf.regs.a5 = pt.regs[9];
        tf.regs.a6 = pt.regs[10];
        tf.regs.a7 = pt.regs[11];
        tf.regs.t0 = pt.regs[12];
        tf.regs.t1 = pt.regs[13];
        tf.regs.t2 = pt.regs[14];
        tf.regs.t3 = pt.regs[15];
        tf.regs.t4 = pt.regs[16];
        tf.regs.t5 = pt.regs[17];
        tf.regs.t6 = pt.regs[18];
        tf.regs.t7 = pt.regs[19];
        tf.regs.t8 = pt.regs[20];
        tf.regs.u0 = pt.regs[21];
        tf.regs.fp = pt.regs[22];
        tf.regs.s0 = pt.regs[23];
        tf.regs.s1 = pt.regs[24];
        tf.regs.s2 = pt.regs[25];
        tf.regs.s3 = pt.regs[26];
        tf.regs.s4 = pt.regs[27];
        tf.regs.s5 = pt.regs[28];
        tf.regs.s6 = pt.regs[29];
        tf.regs.s7 = pt.regs[30];
        tf.regs.s8 = pt.regs[31];
        tf.era = pt.csr_era;
        tf.prmd = pt.csr_prmd;
    }
}

/// Handle a breakpoint exception (INT3/EBREAK/BRK) by dispatching to the
/// kprobe subsystem.
///
/// Converts the trap frame to `PtRegs`, runs the kprobe handler, and writes
/// back any register modifications. Returns `true` if a kprobe was hit and
/// handled, `false` otherwise.
pub fn handle_breakpoint(tf: &mut ax_hal::context::TrapFrame) -> bool {
    let mut pt_regs = trapframe_to_ptregs(tf);
    let handled = with_manager(|manager| kprobe::kprobe_handler_from_break(manager, &mut pt_regs));
    if handled.is_some() {
        ptregs_write_back(&pt_regs, tf);
        return true;
    }
    false
}

/// Handle a debug exception (single-step) by dispatching to the kprobe subsystem.
///
/// Currently only available on x86_64, which is the only architecture that
/// uses hardware single-step for kprobe execution.
#[cfg(target_arch = "x86_64")]
pub fn handle_debug(tf: &mut ax_hal::context::TrapFrame) -> bool {
    let mut pt_regs = trapframe_to_ptregs(tf);
    let handled = with_manager(|manager| kprobe::kprobe_handler_from_debug(manager, &mut pt_regs));
    if handled.is_some() {
        ptregs_write_back(&pt_regs, tf);
        return true;
    }
    false
}

#[allow(dead_code)]
fn flush_tlb_range(start: VirtAddr, size: usize) {
    for offset in (0..size).step_by(PAGE_SIZE_4K) {
        ax_hal::asm::flush_tlb(Some(start + offset));
    }
}
