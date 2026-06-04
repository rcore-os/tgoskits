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

use alloc::sync::Arc;
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
    ebpf::{BPF_HELPER_FUN_SET, error::BpfResultExt, prog::BpfProg},
    file::FileLike,
};

/// Wraps `kbpf_basic::perf::bpf::BpfPerfEvent` with kernel state: a poll
/// set so readers can wait for new records, and the backing pages produced
/// by `device_mmap` (Some after the user `mmap`s the ringbuf; None before).
///
/// Field order is load-bearing: `inner` holds a raw pointer into the page
/// buffer for the lifetime of the `RingPage`; `pages` owns the allocation.
/// Rust drops fields in declaration order, so `inner` (the borrower) goes
/// before `pages` (the owner), and the buffer is freed only after the last
/// access through `inner` is gone.
///
/// `pages` is refcounted (`Arc`): `device_mmap` hands a second strong ref
/// back through `DeviceMmap::Physical`'s retainer slot, which the resulting
/// user VMA holds onto. The backing pages therefore outlive `close(perf_fd)`
/// (which drops this wrapper) for as long as the user mapping is live, so a
/// userspace read of the ringbuf after closing the fd — or a later reuse of
/// those frames — can never observe freed memory.
pub struct BpfPerfEventWrapper {
    inner: BpfPerfEvent,
    poll_ready: PollSet,
    /// MUST be declared after `inner`. Holds the contiguous pages backing
    /// the ringbuf, refcounted so a user VMA keeps them alive across perf-fd
    /// close; the frames return to the global page allocator only once both
    /// this wrapper and every mapping built from it are gone.
    pages: Option<Arc<GlobalPage>>,
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

    /// Write a record into the ringbuf and wake any readers. Pre-mmap
    /// calls are accepted as no-ops: `kbpf_basic::RingPage` is in its
    /// `empty()` state and writing through its null `ptr` would be UB.
    pub fn write_event(&mut self, data: &[u8]) -> AxResult<()> {
        if self.pages.is_none() {
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
        if self.pages.is_some() {
            // Linux allows only one mmap per perf event fd; a second call
            // would orphan the first user mapping if we swapped the pages.
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
        // Hand a second strong ref back to the caller, which threads it into
        // `DeviceMmap::Physical`'s retainer so the user VMA pins these frames
        // until `munmap`/exit even if the perf fd (and this wrapper) is closed
        // first. Without the anchor the pages would free under a live mapping.
        let anchor: Arc<dyn Any + Send + Sync> = pages.clone();
        self.pages = Some(pages);
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

/// A loaded BPF program bundled with an `rbpf` interpreter that borrows
/// into the program's instruction buffer.
///
/// Soundness: the interpreter holds a `'static`-typed slice into the
/// instruction bytes owned by `_prog`; the only thing keeping those bytes
/// alive is the [`Arc<BpfProg>`] in `_prog`. Field order in this struct is
/// therefore load-bearing — `vm` is declared first, `_prog` last, so the
/// struct's drop glue runs `vm`'s destructor before `_prog`'s, and the
/// instruction buffer is freed strictly after the borrower is gone. Do not
/// reorder the fields.
pub struct OwnedEbpfVm {
    vm: EbpfVmRaw<'static>,
    /// MUST be declared after `vm` (drop order). Keeps the instruction
    /// buffer alive for the entire lifetime of `vm`.
    _prog: Arc<BpfProg>,
}

impl OwnedEbpfVm {
    /// Build an `rbpf::EbpfVmRaw` around the program's instruction stream
    /// and register the kernel helper table on it. The returned value owns
    /// both the VM and the [`Arc<BpfProg>`] backing its instruction buffer.
    pub fn new(bpf_prog: Arc<dyn FileLike>) -> AxResult<Self> {
        let prog = bpf_prog
            .into_any_arc()
            .downcast::<BpfProg>()
            .map_err(|_| AxError::InvalidInput)?;
        // Extend the borrow of `prog.insns()` to `'static`. SAFETY: the
        // Arc<BpfProg> is moved into the returned `OwnedEbpfVm` together
        // with the VM, and the struct's field drop order (vm before _prog)
        // guarantees the borrower is destroyed before the buffer is freed.
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
        // TODO: not all of the address space is accessible to a BPF program;
        // allowing the full `0..u64::MAX` range disables rbpf's bounds check
        // and lets a buggy/hostile program read arbitrary kernel memory via
        // direct loads. Narrow this to the legitimately-reachable context /
        // map / stack ranges once kbpf-basic exposes the per-program bounds.
        vm.register_allowed_memory(0..u64::MAX);

        Ok(Self { vm, _prog: prog })
    }

    /// Execute the wrapped BPF program with the supplied context bytes.
    ///
    /// Takes `&self`: `rbpf::EbpfVmRaw::execute_program` is itself `&self`
    /// (the interpreter keeps its scratch state on the local stack), so no
    /// exterior mutability — and therefore no lock — is required around an
    /// `OwnedEbpfVm`.
    pub fn execute_program(&self, ctx: &mut [u8]) -> Result<u64, rbpf::lib::Error> {
        self.vm.execute_program(ctx)
    }

    /// Execute the wrapped BPF program with a `PtRegs` as the single-pointer
    /// context argument the kprobe/kretprobe ABI expects.
    pub fn execute_with_ptregs(&self, pt_regs: &mut PtRegs) -> Result<u64, rbpf::lib::Error> {
        // SAFETY: kbpf-basic's kprobe-context contract passes a raw
        // pointer to `PtRegs` as the program context; we hand the same
        // bytes here.
        let probe_context = unsafe {
            core::slice::from_raw_parts_mut(
                pt_regs as *mut PtRegs as *mut u8,
                core::mem::size_of::<PtRegs>(),
            )
        };
        self.vm.execute_program(probe_context)
    }
}

impl Debug for OwnedEbpfVm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "OwnedEbpfVm")
    }
}

// SAFETY: the bundled `EbpfVmRaw<'static>` is an interpreter over an immutable
// instruction slice; the `Arc<BpfProg>` backing that slice is `Send + Sync`.
// `execute_program` runs entirely off `&self` and a private stack, so it is
// re-entrant and may be driven concurrently from probe-fire paths on several
// CPUs without data races. The raw-pointer fields rbpf carries internally are
// never mutated after construction, so promoting the bundle to `Send + Sync`
// is sound.
unsafe impl Send for OwnedEbpfVm {}
unsafe impl Sync for OwnedEbpfVm {}
