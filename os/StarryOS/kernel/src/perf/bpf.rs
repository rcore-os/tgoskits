//! Software perf event with BPF program attachment + ringbuf output.
//!
//! The user-visible ringbuf is created on the first `mmap(perf_fd, ...)`
//! call: `BpfPerfEventWrapper::device_mmap` allocates `1 + 2^N` physically
//! contiguous 4 K pages (header page + power-of-two-page data ring) and
//! hands the kernel virtual address to `BpfPerfEvent::do_mmap`, which
//! initializes `perf_event_mmap_page` in page 0. `sys_mmap` then maps the
//! same physical range into the caller's address space, so user reads of
//! `data_head` / `data_tail` and kernel writes via `bpf_perf_event_output`
//! share one buffer.

use alloc::sync::{Arc, Weak};
use core::{any::Any, fmt::Debug};

use ax_alloc::GlobalPage;
use ax_errno::{AxError, AxResult};
use ax_hal::mem::virt_to_phys;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use axpoll::{IoEvents, PollSet, Pollable};
use kbpf_basic::{
    linux_bpf::perf_event_sample_format,
    perf::{PerfProbeArgs, bpf::BpfPerfEvent},
};
use kprobe::PtRegs;
use rbpf::EbpfVmRaw;

use super::PerfEventOps;
use crate::{
    ebpf::{BPF_HELPER_FUN_SET, bpf_insn::BpfInsn, ebpf_jit, error::BpfResultExt, prog::BpfProg},
    file::FileLike,
};

/// Wraps `kbpf_basic::perf::bpf::BpfPerfEvent` with kernel state: a poll
/// set so readers can wait for new records, and a weak handle to the
/// backing pages produced by `device_mmap` (Some after the first
/// `mmap(perf_fd)`; None before).
///
/// Ownership model: the user VMA owns the ringbuf pages via the strong
/// `Arc<GlobalPage>` threaded into `DeviceMmap::Physical`'s retainer slot;
/// this wrapper keeps only a `Weak`. Consequences:
///
/// * UAF safety — the pages outlive `close(perf_fd)` (which drops this
///   wrapper) for as long as a mapping is live, because the VMA holds the
///   strong ref. A userspace read after closing the fd never observes
///   freed memory.
/// * Self-cleaning allocation — if a `device_mmap` result is never adopted
///   by a surviving VMA (a non-direct mmap path, a permission/address
///   error, or an `aspace.map` failure), the lone strong ref drops, the
///   frames free, and `is_mapped` flips back to false so the perf fd can
///   be mmap'd again instead of being wedged in `ResourceBusy`. After a
///   normal `munmap` the same thing happens, matching Linux's allowance to
///   re-`mmap` a perf fd.
///
/// `inner` holds a raw pointer into the page buffer; `RingPage` has no
/// destructor and is never dereferenced once the pages are gone (every
/// access through `inner` is gated on [`Self::is_mapped`]), so a dangling
/// pointer left after the pages free is harmless.
pub struct BpfPerfEventWrapper {
    inner: BpfPerfEvent,
    poll_ready: PollSet,
    /// Weak handle to the contiguous pages backing the ringbuf. The strong
    /// ref(s) live in the user VMA(s); `strong_count() > 0` means a live
    /// mapping still exists. See the type-level docs for the ownership
    /// rationale.
    pages: Option<Weak<GlobalPage>>,
}

impl BpfPerfEventWrapper {
    /// Construct the wrapper around a freshly-built `BpfPerfEvent`.
    pub fn new(inner: BpfPerfEvent) -> Self {
        Self {
            inner,
            poll_ready: PollSet::new(),
            pages: None,
        }
    }

    /// Whether a live user mapping of the ringbuf currently exists. The
    /// wrapper only holds a `Weak` to the backing pages, so this is true
    /// exactly while some VMA still pins them; once every mapping is gone
    /// (munmap / exit) — or an in-progress mmap was abandoned before a VMA
    /// adopted the pages — the strong refs drop and this returns false.
    fn is_mapped(&self) -> bool {
        self.pages.as_ref().is_some_and(|w| w.strong_count() > 0)
    }

    /// Write a record into the ringbuf and wake any readers. Calls before a
    /// mapping exists (or after it is gone) are accepted as no-ops: the
    /// `kbpf_basic::RingPage` pointer is either still `empty()` or now
    /// dangling, so dereferencing it would be UB.
    pub fn write_event(&mut self, data: &[u8]) -> AxResult<()> {
        if !self.is_mapped() {
            return Ok(());
        }
        self.inner.write_event(data).into_ax_result()?;
        if self.inner.enabled() {
            self.poll_ready.wake();
        }
        Ok(())
    }
}

impl Debug for BpfPerfEventWrapper {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BpfPerfEventWrapper")
    }
}

impl PerfEventOps for BpfPerfEventWrapper {
    fn enable(&mut self) -> AxResult<()> {
        self.inner.enable().into_ax_result()?;
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        self.inner.disable().into_ax_result()?;
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn device_mmap(&mut self, len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        if self.is_mapped() {
            // Linux allows only one live mmap per perf event fd; a second
            // mapping while the first is alive would orphan it. A stale
            // `Weak` from an abandoned or munmap'd previous attempt does not
            // count (its pages are already freed), so the fd stays mmap-able.
            return Err(AxError::ResourceBusy);
        }
        // libbpf requires `(1 + 2^N) * PAGE_SIZE` so the data region is a
        // power of two pages; `RingPage::init` enforces ≥ 2 pages total and
        // 4 K alignment. Reject anything that would trip those asserts.
        if len == 0 || !len.is_multiple_of(PAGE_SIZE_4K) {
            return Err(AxError::InvalidInput);
        }
        let num_pages = len / PAGE_SIZE_4K;
        if num_pages < 2 || !(num_pages - 1).is_power_of_two() {
            return Err(AxError::InvalidInput);
        }
        let mut pages = GlobalPage::alloc_contiguous(num_pages, PAGE_SIZE_4K)?;
        pages.zero();
        let kvirt = pages.start_vaddr();
        let paddr = virt_to_phys(kvirt);
        self.inner
            .do_mmap(kvirt.as_usize(), len, 0)
            .map_err(|_| AxError::InvalidInput)?;
        let pages = Arc::new(pages);
        // Keep only a `Weak`; hand the sole strong ref to the caller, which
        // threads it into `DeviceMmap::Physical`'s retainer so the user VMA
        // pins these frames until `munmap`/exit even if the perf fd (and this
        // wrapper) is closed first. Because the wrapper does not retain a
        // strong ref, an mmap that is abandoned or fails before a VMA adopts
        // the anchor simply frees the pages and leaves the fd mmap-able again
        // (see the type-level docs). Without the anchor the pages would free
        // under a live mapping.
        self.pages = Some(Arc::downgrade(&pages));
        let anchor: Arc<dyn Any + Send + Sync> = pages;
        Ok((paddr, anchor))
    }
}

impl Pollable for BpfPerfEventWrapper {
    fn poll(&self) -> axpoll::IoEvents {
        if self.inner.readable() {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: axpoll::IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_ready.register(context.waker());
        }
    }
}

/// Build a `BpfPerfEventWrapper` from `perf_event_open` args. The upstream
/// code asserts `sample_type == PERF_SAMPLE_RAW`; we keep that assertion
/// to match the verifier contract and surface bad input early.
pub fn perf_event_open_bpf(args: PerfProbeArgs) -> BpfPerfEventWrapper {
    debug_assert_eq!(
        args.sample_type,
        Some(perf_event_sample_format::PERF_SAMPLE_RAW)
    );
    BpfPerfEventWrapper::new(BpfPerfEvent::new(args))
}

/// Execution backend for a loaded BPF program: JIT-compiled native code
/// or the `rbpf` interpreter as fallback.
enum EbpfExecutor {
    /// JIT-compiled native code.
    Jit {
        entry: unsafe extern "C" fn(*mut u8) -> u64,
        _jit_buf: ebpf_jit::JitBuffer,
    },
    /// Interpreted execution via `rbpf::EbpfVmRaw`.
    Interpreter(EbpfVmRaw<'static>),
}

/// A loaded BPF program bundled with an execution backend (JIT or
/// interpreter).
///
/// Soundness: both the interpreter and the JIT buffer reference the
/// instruction bytes owned by `_prog`. Field order is load-bearing —
/// `executor` is declared first, `_prog` last, so the struct's drop glue
/// runs the executor's destructor before `_prog`'s.
/// Do not reorder the fields.
pub struct OwnedEbpfVm {
    executor: EbpfExecutor,
    /// MUST be declared after `executor` (drop order).
    _prog: Arc<BpfProg>,
}

impl OwnedEbpfVm {
    /// Build an execution backend for the BPF program. Tries JIT
    /// compilation first; falls back to the `rbpf` interpreter.
    pub fn new(bpf_prog: Arc<dyn FileLike>) -> AxResult<Self> {
        let prog = bpf_prog
            .into_any_arc()
            .downcast::<BpfProg>()
            .map_err(|_| AxError::InvalidInput)?;

        let executor = if let Some(jit_executor) = Self::try_jit(&prog) {
            jit_executor
        } else {
            Self::build_interpreter(&prog)?
        };

        Ok(Self {
            executor,
            _prog: prog,
        })
    }

    fn try_jit(prog: &Arc<BpfProg>) -> Option<EbpfExecutor> {
        let prog_slice = prog.insns();
        if !prog_slice
            .len()
            .is_multiple_of(core::mem::size_of::<BpfInsn>())
        {
            warn!("eBPF JIT: bytecode length not aligned to BpfInsn size");
            return None;
        }
        let insn_count = prog_slice.len() / core::mem::size_of::<BpfInsn>();
        if insn_count == 0 {
            return None;
        }
        // SAFETY: BpfInsn is #[repr(C)] and 8 bytes; the byte slice is
        // byte-swapped (little-endian) by kbpf-basic preprocessor.
        let insns = unsafe {
            core::slice::from_raw_parts(prog_slice.as_ptr() as *const BpfInsn, insn_count)
        };
        let helpers = BPF_HELPER_FUN_SET.get()?;
        let jit_buf = ebpf_jit::try_jit_compile(insns, helpers)?;
        // SAFETY: the JIT buffer is page-aligned and holds valid native
        // code for the target architecture.
        let entry: unsafe extern "C" fn(*mut u8) -> u64 =
            unsafe { core::mem::transmute(jit_buf.entry()) };
        info!(
            "eBPF JIT: compiled {} instructions into {} bytes of native code",
            insn_count,
            jit_buf.offset()
        );
        Some(EbpfExecutor::Jit {
            entry,
            _jit_buf: jit_buf,
        })
    }

    fn build_interpreter(prog: &Arc<BpfProg>) -> AxResult<EbpfExecutor> {
        let prog_slice = prog.insns();
        let prog_slice =
            unsafe { core::slice::from_raw_parts(prog_slice.as_ptr(), prog_slice.len()) };
        let mut vm = EbpfVmRaw::new(Some(prog_slice)).map_err(|e| {
            error!("rbpf::EbpfVmRaw::new failed: {e:?}");
            AxError::InvalidInput
        })?;
        if let Some(table) = BPF_HELPER_FUN_SET.get() {
            for (key, value) in table.iter() {
                let _ = vm.register_helper(*key, *value);
            }
        }
        vm.register_allowed_memory(0..u64::MAX);
        Ok(EbpfExecutor::Interpreter(vm))
    }

    pub fn execute_program(&self, ctx: &mut [u8]) -> Result<u64, rbpf::lib::Error> {
        match &self.executor {
            EbpfExecutor::Jit { entry, .. } => Ok(unsafe { entry(ctx.as_mut_ptr()) }),
            EbpfExecutor::Interpreter(vm) => vm.execute_program(ctx),
        }
    }

    pub fn execute_with_ptregs(&self, pt_regs: &mut PtRegs) -> Result<u64, rbpf::lib::Error> {
        match &self.executor {
            EbpfExecutor::Jit { entry, .. } => {
                Ok(unsafe { entry(pt_regs as *mut PtRegs as *mut u8) })
            }
            EbpfExecutor::Interpreter(vm) => {
                let probe_context = unsafe {
                    core::slice::from_raw_parts_mut(
                        pt_regs as *mut PtRegs as *mut u8,
                        core::mem::size_of::<PtRegs>(),
                    )
                };
                vm.execute_program(probe_context)
            }
        }
    }
}

impl Debug for OwnedEbpfVm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.executor {
            EbpfExecutor::Jit { .. } => write!(f, "OwnedEbpfVm(jit)"),
            EbpfExecutor::Interpreter(_) => write!(f, "OwnedEbpfVm(interp)"),
        }
    }
}

// SAFETY: both execution backends operate over an immutable instruction
// slice / JIT buffer backed by the `Arc<BpfProg>`; `execute_program` runs
// entirely off `&self` and a private stack, so it is re-entrant and may be
// driven concurrently from probe-fire paths on several CPUs without data
// races. The JIT code is read-only after compilation.
unsafe impl Send for OwnedEbpfVm {}
unsafe impl Sync for OwnedEbpfVm {}
