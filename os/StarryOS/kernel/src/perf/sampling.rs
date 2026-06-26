//! PMU overflow-IRQ sampling backend (M2: `perf record` via `PERF_SAMPLE_IP`).
//!
//! This is the IRQ half of hardware-PMU sampling. A sampling perf event
//! ([`super::hw::HwPerfEvent`] with `sample_period > 0`) preloads a programmable
//! counter so it overflows after `period` events; the overflow raises the PMUv3
//! interrupt (PPI 7 / INTID 23). [`pmu_overflow_handler`] runs in hard-IRQ
//! context, reads the interrupted PC, builds one `PERF_RECORD_SAMPLE` per
//! overflowed counter, writes it into that event's mmap ring buffer, re-arms the
//! counter, and wakes a deferred worker (via [`ax_task::IrqNotify`]) that
//! delivers `POLLIN` to userspace pollers.
//!
//! IRQ-context discipline (enforced throughout this module's handler path):
//! no allocation, no sleeping locks, and the interrupted `ELR_EL1` / `SPSR_EL1`
//! are read *first* (before touching the PMU or memory) so a nested fault can
//! never clobber them.
//!
//! # Per-CPU registry
//!
//! The handler must locate the ring buffer for an overflowed counter `n` without
//! allocating or taking a lock. [`REGISTRY`] is a fixed `[Option<SampleSlot>; 32]`
//! per CPU (index = programmable counter index). A [`SampleSlot`] is a small
//! `Copy` POD carrying exactly the raw values the handler needs. `register` /
//! `unregister` mutate the *current* CPU's array under a local-IRQ-off critical
//! section ([`NoPreemptIrqSave`]) so they never race the handler. M2 is
//! single-core, so the event's core is always cpu0.
//!
//! # `notify` raw pointer soundness
//!
//! `SampleSlot::notify` is a raw `*const IrqNotify`. It is valid for the whole
//! time the slot is registered because the owning event holds a strong
//! `Arc<IrqNotify>` for its entire life, and teardown
//! ([`super::hw::HwPerfEvent`]'s disable/Drop) calls [`unregister`] — clearing
//! the slot — *before* dropping that `Arc`. The handler therefore only ever
//! dereferences a pointer whose target is still alive.

use core::{ptr::NonNull, sync::atomic::Ordering};

use ax_hal::irq::{IrqContext, IrqReturn};
use ax_kernel_guard::NoPreemptIrqSave;
use ax_task::IrqNotify;
use kbpf_basic::linux_bpf::perf_event_mmap_page;

/// Architecturally fixed PMUv3 overflow interrupt INTID.
///
/// On ARMv8 the PMU overflow interrupt is wired as PPI 7, i.e. GIC INTID 23
/// (`16 + 7`). This is hardcoded for M2: the proper path is to read the
/// `interrupts` property of the FDT `arm,armv8-pmuv3` node, which is a deferred
/// follow-up. On the RK3588 (and the QEMU `virt` machine) the PMU PPI is INTID
/// 23, matching this constant.
const PMU_IRQ: usize = 23;

/// Maximum programmable counter index (matches [`ax_cpu::pmu::counter`] /
/// [`ax_cpu::pmu::overflow`]); the registry is sized one past this for indexing.
const MAX_COUNTER: usize = 30;

/// `PERF_RECORD_SAMPLE` discriminant (`perf_event_type::PERF_RECORD_SAMPLE`).
const PERF_RECORD_SAMPLE: u32 = 9;
/// `PERF_RECORD_MISC_KERNEL`: the sample landed in kernel (EL1) context.
const PERF_RECORD_MISC_KERNEL: u16 = 1;
/// `PERF_RECORD_MISC_USER`: the sample landed in user (EL0) context.
const PERF_RECORD_MISC_USER: u16 = 2;

/// Size of an M2 `PERF_SAMPLE_IP` record: 8-byte header + one 8-byte `ip`.
const SAMPLE_RECORD_LEN: usize = 16;

/// `perf_event_sample_format::PERF_SAMPLE_IP`. The only `sample_type` M2 emits.
const PERF_SAMPLE_IP: u64 = 1;

/// Everything the overflow handler needs for one counter, in a lock-free,
/// alloc-free `Copy` POD.
///
/// Stored in the per-CPU [`REGISTRY`] at the counter's index while the event is
/// enabled. See the module docs for the `notify`-pointer soundness argument.
#[derive(Clone, Copy)]
pub struct SampleSlot {
    /// Kernel virtual address of the ring buffer's first page
    /// (`perf_event_mmap_page`). The data region follows at `data_offset`.
    pub ring_vaddr: usize,
    /// Total ring length in bytes (header page + data region).
    pub ring_len: usize,
    /// Sampling period: the counter is re-armed to overflow after this many
    /// events via [`ax_cpu::pmu::counter::preload`].
    pub period: u32,
    /// `attr.sample_type`. For M2 this is exactly `PERF_SAMPLE_IP`.
    pub sample_type: u64,
    /// Raw pointer to the owning event's [`IrqNotify`], woken after each sample.
    /// Kept alive by the event's strong `Arc<IrqNotify>` for as long as the slot
    /// is registered (see module docs).
    pub notify: *const (),
}

// SAFETY: `SampleSlot` is a plain bag of integers plus a raw pointer. The
// pointer is only ever dereferenced from the overflow handler on the same CPU
// that registered the slot, and the registry is mutated only under a
// local-IRQ-off critical section, so there is no cross-thread aliasing of the
// pointee through this type. Marking it `Send` lets it live inside the per-CPU
// static; it is never actually moved across CPUs (single-core in M2, and the
// registry is per-CPU regardless).
unsafe impl Send for SampleSlot {}

/// Per-CPU map from programmable counter index to its registered sampling slot.
///
/// Index `n` (`0..=30`) holds the slot for `PMEVCNTRn_EL0`. `None` means no
/// sampling event currently owns that counter on this CPU.
#[ax_percpu::def_percpu]
static REGISTRY: [Option<SampleSlot>; 32] = [None; 32];

/// Whether [`pmu_overflow_handler`] has been registered with the IRQ framework.
///
/// Registration is process-global and idempotent: the handler walks the per-CPU
/// registry, so a single action installed on all CPUs suffices.
static REGISTERED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Registers `slot` for programmable counter `n` on the current CPU.
///
/// Runs in process context on the event's core (cpu0 under smp1). The mutation
/// is performed under [`NoPreemptIrqSave`] so the overflow handler — which reads
/// the same per-CPU array — can never observe a half-written entry, and so the
/// current CPU's view of `REGISTRY` is the one being updated.
pub fn register(n: usize, slot: SampleSlot) {
    if n > MAX_COUNTER {
        return;
    }
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: preemption and local IRQs are disabled by `_guard`, so we hold
    // exclusive access to this CPU's `REGISTRY` for the critical section.
    let registry = unsafe { REGISTRY.current_ref_mut_raw() };
    registry[n] = Some(slot);
}

/// Clears the sampling slot for programmable counter `n` on the current CPU.
///
/// Mirror of [`register`]. Teardown calls this *before* the owning event drops
/// its `Arc<IrqNotify>`, so once this returns the handler can no longer reach a
/// stale `notify` pointer for counter `n`.
pub fn unregister(n: usize) {
    if n > MAX_COUNTER {
        return;
    }
    let _guard = NoPreemptIrqSave::new();
    // SAFETY: see `register`.
    let registry = unsafe { REGISTRY.current_ref_mut_raw() };
    registry[n] = None;
}

/// Ensures [`pmu_overflow_handler`] is registered with the IRQ framework and the
/// PMU overflow line is enabled on the current core.
///
/// Idempotent: the first caller installs the per-CPU action for [`PMU_IRQ`]
/// across all online CPUs. Every caller (re-)enables INTID 23 on the *current*
/// core. The explicit `set_enable` is required: the framework's per-core line
/// enable runs at `cpu_online`/boot, before this handler is ever registered, so
/// under smp1 the PMU PPI would otherwise stay masked and the overflow IRQ would
/// never fire on cpu0.
pub fn ensure_pmu_irq_registered() {
    if REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let cpus = ax_hal::irq::CpuMask::first_n(ax_hal::cpu_num());
        // Mirror the timer's unit-data pattern: the handler does not use `data`.
        if let Err(err) = ax_hal::irq::request_percpu_irq(
            PMU_IRQ,
            cpus,
            pmu_overflow_handler,
            NonNull::dangling(),
        ) {
            // Roll back so a later open can retry registration.
            REGISTERED.store(false, Ordering::Release);
            warn!("perf sampling: failed to register PMU overflow IRQ: {err:?}");
            return;
        }
    }
    // Enable the PMU PPI on the core this sampling event runs on. Required even
    // when the action was registered by an earlier event: the per-core line is
    // not auto-enabled for runtime-registered PPIs.
    ax_hal::irq::set_enable(PMU_IRQ, true);
}

/// PMU overflow IRQ handler (hard-IRQ context).
///
/// Reads the interrupted PC and EL *first*, then services every overflowed
/// programmable counter that has a registered sampling slot: builds a
/// `PERF_RECORD_SAMPLE`, writes it into the event's ring, re-arms the counter,
/// and wakes the event's deferred worker. Clears only the overflow bits it
/// actually serviced (write-1-to-clear) at the end.
///
/// Returns [`IrqReturn::Handled`] if any counter overflowed (whether or not a
/// slot was registered for it), else [`IrqReturn::Unhandled`].
///
/// # Safety
///
/// Must only be invoked by the IRQ framework in hard-IRQ context on the core the
/// overflow fired on. Performs no allocation and takes no sleeping locks.
pub unsafe fn pmu_overflow_handler(_ctx: IrqContext, _data: NonNull<()>) -> IrqReturn {
    // Capture the interrupted context before doing anything that could fault or
    // overwrite ELR_EL1 / SPSR_EL1.
    let ip = ax_cpu::pmu::interrupted_pc();
    let is_user = ax_cpu::pmu::interrupted_is_user();

    let ovf = ax_cpu::pmu::overflow::status();
    if ovf == 0 {
        return IrqReturn::Unhandled;
    }

    let misc = if is_user {
        PERF_RECORD_MISC_USER
    } else {
        PERF_RECORD_MISC_KERNEL
    };

    // Bits we have serviced; cleared (write-1-to-clear) at the very end so a
    // counter is not re-armed and re-cleared in a way that drops a concurrent
    // overflow we have not looked at.
    let mut handled: u32 = 0;

    for n in 0..=MAX_COUNTER {
        if ovf & (1 << n) == 0 {
            continue;
        }
        handled |= 1 << n;

        // SAFETY: we run on the core that took the IRQ with local IRQs masked,
        // so this CPU's `REGISTRY` is not being mutated concurrently (register /
        // unregister disable local IRQs).
        let slot = unsafe { REGISTRY.current_ref_mut_raw() }[n];
        let Some(slot) = slot else {
            // Overflow on a counter with no sampling slot (e.g. a counting-only
            // event that happened to wrap with its IRQ somehow set): just clear
            // it below. Do not re-arm — counting events manage their own value.
            continue;
        };

        // M2 supports exactly PERF_SAMPLE_IP; `hw.rs` rejects anything else at
        // open time. Re-check defensively here (the slot carries `sample_type`)
        // so a future non-IP sample_type never produces a malformed record: skip
        // building a record but still re-arm and clear the overflow below.
        if slot.sample_type == PERF_SAMPLE_IP {
            // Record layout is fixed: header{type=9, misc, size=16} + ip:u64.
            let mut record = [0u8; SAMPLE_RECORD_LEN];
            record[0..4].copy_from_slice(&PERF_RECORD_SAMPLE.to_ne_bytes());
            record[4..6].copy_from_slice(&misc.to_ne_bytes());
            record[6..8].copy_from_slice(&(SAMPLE_RECORD_LEN as u16).to_ne_bytes());
            record[8..16].copy_from_slice(&ip.to_ne_bytes());

            // SAFETY: `ring_vaddr`/`ring_len` describe live, kernel-mapped pages
            // for as long as the slot is registered (the event pins them, and
            // teardown unregisters before freeing). `ring_write` only touches
            // that region.
            unsafe { ring_write(slot.ring_vaddr, slot.ring_len, &record) };
        }

        // Re-arm the counter for the next sample.
        ax_cpu::pmu::counter::preload(n, slot.period);

        // Wake the deferred worker so it can deliver POLLIN. The pointer is
        // valid: the event holds the backing `Arc<IrqNotify>` while registered.
        // SAFETY: see the module-level `notify` soundness note.
        let notify = unsafe { &*(slot.notify as *const IrqNotify) };
        notify.notify_irq();
    }

    // Clear exactly the overflow bits we serviced.
    ax_cpu::pmu::overflow::clear(handled);
    IrqReturn::Handled
}

/// Writes one record into a perf ring buffer, IRQ-safe and self-contained.
///
/// Page 0 of `[ring_vaddr, ring_vaddr + ring_len)` is a
/// [`perf_event_mmap_page`]; the data region starts at `ring_vaddr + data_offset`
/// (`data_offset == PAGE_SIZE` for our buffers) and is `data_size` bytes. The
/// record is copied at `data_head % data_size` (split into two copies on wrap),
/// then `data_head` is published with a release fence so a userspace reader that
/// observes the new `data_head` also observes the bytes.
///
/// If the record would overwrite still-unread bytes
/// (`data_head - data_tail + len > data_size`) it is dropped: `data_head` is not
/// advanced. Lost-record accounting is intentionally omitted for M2.
///
/// # Safety
///
/// `ring_vaddr` must point at a live, kernel-mapped ring of `ring_len` bytes
/// (header page + data region) whose header was initialized by
/// `HwPerfEvent::device_mmap`. The caller must ensure no concurrent kernel
/// writer touches the same ring (guaranteed here: one counter ⇒ one writer, and
/// the handler runs with local IRQs masked).
unsafe fn ring_write(ring_vaddr: usize, ring_len: usize, record: &[u8]) {
    // Guard the enable-before-mmap case (slot registered with a zero ring) and
    // any ring too small to even hold the header page: there is nowhere to
    // write, and the header pointer would be null/out of bounds.
    if ring_vaddr == 0 || ring_len < core::mem::size_of::<perf_event_mmap_page>() {
        return;
    }

    let header = ring_vaddr as *mut perf_event_mmap_page;

    // SAFETY: `header` points at the initialized header page.
    let data_offset =
        unsafe { core::ptr::addr_of!((*header).data_offset).read_volatile() } as usize;
    let data_size = unsafe { core::ptr::addr_of!((*header).data_size).read_volatile() } as usize;

    // Defensive: a malformed/zero header (no data region, or a data window that
    // does not fit in the buffer) means there is nowhere safe to write.
    if data_size == 0 || data_offset > ring_len || data_offset + data_size > ring_len {
        return;
    }

    let len = record.len();
    if len > data_size {
        return;
    }

    // SAFETY: header page is initialized; these are plain u64 fields.
    let head = unsafe { core::ptr::addr_of!((*header).data_head).read_volatile() };
    let tail = unsafe { core::ptr::addr_of!((*header).data_tail).read_volatile() };

    // Would this record overwrite bytes the reader has not consumed yet? Drop it
    // if so (back-pressure; no lost-record accounting in M2).
    if head.wrapping_sub(tail).wrapping_add(len as u64) > data_size as u64 {
        return;
    }

    let data_base = ring_vaddr + data_offset;
    let start = (head % data_size as u64) as usize;
    let first = core::cmp::min(len, data_size - start);

    // SAFETY: `data_base + start + first <= data_base + data_size`, within the
    // mapped data region; same for the wrapped remainder below.
    unsafe {
        core::ptr::copy_nonoverlapping(record.as_ptr(), (data_base + start) as *mut u8, first);
        if first < len {
            core::ptr::copy_nonoverlapping(
                record.as_ptr().add(first),
                data_base as *mut u8,
                len - first,
            );
        }
    }

    // Publish the bytes before the new head: a reader observing the updated
    // `data_head` must also observe the record contents.
    core::sync::atomic::fence(Ordering::Release);
    // SAFETY: header page is initialized.
    unsafe {
        core::ptr::addr_of_mut!((*header).data_head).write_volatile(head.wrapping_add(len as u64));
    }
}
