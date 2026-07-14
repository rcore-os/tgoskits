//! `perf_event_open(2)` runtime: dispatcher across kprobe / tracepoint /
//! software-bpf / uprobe perf event types, the file-like `PerfEvent`
//! wrapper, and the ringbuf output path used by the `bpf_perf_event_output`
//! helper. The `mmap(perf_fd, ...)` path is wired through
//! `FileLike::device_mmap` → `PerfEventOps::device_mmap`, which allocates
//! the backing pages and asks `kbpf_basic` to initialize the
//! `perf_event_mmap_page` header.

pub mod bpf;
pub mod hw;
pub mod kprobe;
/// IRQ-safe no-fault user/kernel memory reader for PMU-sampling FP unwinding.
/// ARM PMUv3 only; walks `TTBR0`/`TTBR1` against the direct map so a bad frame
/// pointer never faults.
#[cfg(target_arch = "aarch64")]
pub mod nofault;
/// Per-CPU hardware-PMU state (allocator, cluster identity). ARM PMUv3 only;
/// the per-core counter pools + cluster classification live here.
#[cfg(target_arch = "aarch64")]
pub mod percpu;
pub mod raw_tracepoint;
/// PMU overflow-IRQ sampling backend (M2). ARM PMUv3 only; the counting and
/// tracing paths are arch-agnostic, but sampling depends on CPU PMU registers.
#[cfg(target_arch = "aarch64")]
pub mod sampling;
/// Side-band records (`PERF_RECORD_COMM`/`MMAP2`/`FORK`/`EXIT`) for `perf report`
/// symbolization. Writes into the sampling ring from process context, so it is
/// gated like `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod sideband;
/// Software events (`PERF_TYPE_SOFTWARE`) as real per-task counters — the default
/// `perf stat` rows (cpu-clock / task-clock / context-switches / cpu-migrations /
/// page-faults). Pure accounting driven by the scheduler + fault hooks, no PMU,
/// so it is arch-independent.
pub mod sw;
/// Per-task hardware-PMU counting (`perf stat -- cmd`, M3). ARM PMUv3 only; the
/// scheduler hooks call into CPU PMU register helpers, so it is gated like
/// `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod task;
/// Per-CPU perf tick driving Tier-2 counter rotation (multiplexing). ARM PMUv3
/// only; registered with the scheduler tick at [`perf_event_init`].
#[cfg(target_arch = "aarch64")]
pub mod tick;
pub mod tracepoint;
/// Frame-pointer call-graph unwinding for PMU sampling (`PERF_SAMPLE_CALLCHAIN`).
/// ARM PMUv3 only; consumes the interrupted frame pointer plumbed through
/// `ax_cpu::pmu` and the alloc-free `axbacktrace::walk_fp` engine.
#[cfg(target_arch = "aarch64")]
pub mod unwind;
pub mod uprobe;

use alloc::{
    borrow::Cow,
    boxed::Box,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    ffi::c_void,
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_io::{Read, Write};
use ax_kspin::{SpinNoPreempt, SpinNoPreemptGuard};
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{paging::MappingFlags, pmu};
use axpoll::Pollable;
pub use bpf::BpfPerfEventWrapper;
use hashbrown::HashMap;
use kbpf_basic::{
    linux_bpf::{PERF_FLAG_FD_CLOEXEC, perf_event_attr},
    perf::{PerfEventIoc, PerfProbeArgs, PerfProbeConfig, PerfTypeId},
};

use crate::{
    ebpf::{error::BpfResultExt, transform::EbpfKernelAuxiliary},
    file::{FileLike, Kstat, add_file_like, get_file_like},
    mm::{VmBytes, VmBytesMut},
    pseudofs::DeviceMmap,
};

/// Monotonic source of per-event `perf` ids (`PERF_EVENT_IOC_ID`,
/// `PERF_SAMPLE_ID`, `read_format`'s `PERF_FORMAT_ID`). Linux assigns every
/// `perf_event` a unique non-zero id; `perf record` reads it back with
/// `PERF_EVENT_IOC_ID` right after `mmap` to build its id→event map, so the
/// value must be unique and stable for the life of the event. Starts at 1 so 0
/// stays reserved for "no id".
static NEXT_PERF_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// `MIDR_EL1` for the cpuid `sysfs`/`procfs` nodes (`/proc/cpuinfo`,
/// `/sys/devices/.../cpuid`, `.../regs/identification/midr_el1`).
///
/// The real register on aarch64 (ARM PMUv3); `0` on other arches, where there is
/// no PMU and the nodes exist only so the layout stays uniform. Centralizes the
/// `#[cfg(target_arch = "aarch64")]` gate so the pseudo-fs call sites stay arch
/// agnostic (and compile under multi-target clippy).
pub fn read_midr_el1() -> u64 {
    pmu::cpu_id_raw().unwrap_or(0)
}

/// Test-only: enable/disable the big.LITTLE cluster parity override (even CPU =
/// `Little`, odd = `Big`) so a homogeneous machine can exercise the cluster
/// logic. Backs `/proc/sys/kernel/perf_test_force_clusters`. No-op off aarch64.
pub fn set_force_clusters(on: bool) {
    #[cfg(target_arch = "aarch64")]
    percpu::set_force_clusters(on);
    #[cfg(not(target_arch = "aarch64"))]
    let _ = on;
}

/// Whether the cluster parity override is enabled (see [`set_force_clusters`]).
pub fn force_clusters_enabled() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        percpu::force_clusters_enabled()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        false
    }
}

/// `ioctl` type byte for the perf-event ioctls (`'$'`).
const PERF_IOC_TYPE: u32 = 0x24;
/// `PERF_EVENT_IOC_SET_OUTPUT` request number (`_IO('$', 5)`).
const PERF_IOC_NR_SET_OUTPUT: u32 = 5;
/// `PERF_EVENT_IOC_ID` request number (`_IOR('$', 7, __u64 *)`).
const PERF_IOC_NR_ID: u32 = 7;

/// Behaviour every perf event implements. Each variant in the dispatcher
/// (kprobe / tracepoint / software-bpf / uprobe / hardware-PMU) provides a
/// `Box<dyn PerfEventOps>` that `PerfEvent` then drives through the file
/// layer (`ioctl`, `mmap`, `read`, etc.).
pub trait PerfEventOps: Pollable + Send + Sync + Debug {
    /// Begin firing into the registered BPF program / ringbuf.
    fn enable(&mut self) -> AxResult<()>;

    /// Stop firing without tearing down the event.
    fn disable(&mut self) -> AxResult<()>;

    /// `Any` upcast (mutable). Used by `perf_event_output` to recover the
    /// concrete `BpfPerfEventWrapper` from a `dyn PerfEventOps`.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Attach a BPF program to this event (`PERF_EVENT_IOC_SET_BPF`).
    fn set_bpf_prog(&mut self, _bpf_prog: Arc<dyn FileLike>) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    /// Allocate the user-visible ringbuf and return its physical start
    /// address (length is the user-supplied mmap length, page-aligned)
    /// together with a retainer that owns the backing pages. The caller
    /// threads the retainer into `DeviceMmap::PhysicalCached(.., Some(anchor))`
    /// so the pages stay live for as long as the user mapping exists, even
    /// after `close(perf_fd)`. Only `bpf::BpfPerfEventWrapper` overrides
    /// this; the other variants (kprobe/tracepoint/raw-tp/uprobe wrappers)
    /// reject `mmap(perf_fd)`.
    fn device_mmap(&mut self, _len: usize) -> AxResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        Err(AxError::Unsupported)
    }

    /// Read the current counter value plus timing, for `read(perf_fd)`.
    ///
    /// Only the hardware-PMU variant ([`hw::HwPerfEvent`]) overrides this;
    /// the tracing variants have no counter to read and keep the default,
    /// so `read(perf_fd)` returns `Unsupported` for them. The returned
    /// [`PerfReadValues`] carries the raw counter value, the enabled/running
    /// times, and the `read_format` that [`PerfEvent::read`] uses to decide
    /// which of those fields to serialize.
    fn read_values(&mut self) -> AxResult<PerfReadValues> {
        Err(AxError::Unsupported)
    }

    /// Reset the counter to zero (`PERF_EVENT_IOC_RESET`).
    ///
    /// Only the hardware-PMU variant ([`hw::HwPerfEvent`]) overrides this;
    /// the tracing variants keep the default and reject the ioctl.
    fn reset(&mut self) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    /// Record the unique event id this event emits in its `PERF_SAMPLE_ID` /
    /// `PERF_SAMPLE_IDENTIFIER` sample fields. Called once by [`PerfEvent::new`]
    /// with the same id `PERF_EVENT_IOC_ID` reports, so a reader can demultiplex
    /// the events sharing one ring (`perf record -e a,b`). Default no-op: the
    /// tracing variants emit no hardware samples.
    fn set_sample_id(&mut self, _id: u64) {}

    /// Expose this event's mmap ring so another event can redirect its records
    /// into it (`PERF_EVENT_IOC_SET_OUTPUT`, target side).
    ///
    /// Returns `(ring_vaddr, ring_len, anchor)` where `anchor` is a strong
    /// reference pinning the ring's backing pages for as long as the redirecting
    /// event holds it. Only a mapped hardware-PMU sampling event
    /// ([`hw::HwPerfEvent`]) returns `Some`; everything else has no ring to share.
    fn output_ring(&self) -> Option<(usize, usize, Arc<dyn Any + Send + Sync>)> {
        None
    }

    /// Redirect this event's `PERF_RECORD_SAMPLE` output into `ring_vaddr` /
    /// `ring_len` (another event's ring, from its [`output_ring`](Self::output_ring)),
    /// pinning it via `anchor` (`PERF_EVENT_IOC_SET_OUTPUT`, source side).
    ///
    /// The default accepts as a no-op: events that produce no ring records (the
    /// `PERF_COUNT_SW_DUMMY` tracking event `perf record` redirects, the tracing
    /// variants) need no actual redirect. Only [`hw::HwPerfEvent`] sampling events
    /// override this to make their overflow handler write into the shared ring.
    fn redirect_output(
        &mut self,
        _ring_vaddr: usize,
        _ring_len: usize,
        _anchor: Arc<dyn Any + Send + Sync>,
    ) -> AxResult<()> {
        Ok(())
    }
}

/// `read_format` bit selecting `time_enabled` in `read(perf_fd)`.
const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
/// `read_format` bit selecting `time_running` in `read(perf_fd)`.
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
/// `read_format` bit selecting the per-event `id` in `read(perf_fd)`.
const PERF_FORMAT_ID: u64 = 1 << 2;
/// `read_format` bit selecting the per-event lost-sample count in `read(perf_fd)`
/// (`PERF_FORMAT_LOST`, Linux 5.19+). `perf record` sets it so its
/// `record__read_lost_samples` can total samples the ring dropped; the `u64` is
/// appended last, after `id`. Without it, `perf record` prints "read LOST count
/// failed" because the read returns a short buffer.
const PERF_FORMAT_LOST: u64 = 1 << 4;
/// `read_format` bit: a leader read returns the WHOLE group's counters in one
/// buffer (`perf stat -e '{a,b}'`). Layout: `u64 nr`, then `time_enabled` /
/// `time_running` once (if requested), then per event (leader first) `value`
/// plus `id` (`PERF_FORMAT_ID`) / `lost` (`PERF_FORMAT_LOST`).
const PERF_FORMAT_GROUP: u64 = 1 << 3;

/// Counter snapshot returned by [`PerfEventOps::read_values`].
///
/// Mirrors the fields Linux's `read(perf_fd)` can emit, gated by
/// `read_format`. M1 supports `value`, `time_enabled`, `time_running`, and
/// `id`, but not `PERF_FORMAT_GROUP`.
pub struct PerfReadValues {
    /// The raw counter value.
    pub value: u64,
    /// Wall time the event has been enabled, in nanoseconds.
    pub time_enabled: u64,
    /// Wall time the event was scheduled onto hardware, in nanoseconds.
    /// Equal to `time_enabled` in M1 (no multiplexing).
    pub time_running: u64,
    /// `attr.read_format`, controlling which fields [`PerfEvent::read`] emits.
    /// The `PERF_FORMAT_ID` value itself comes from the owning [`PerfEvent`]'s
    /// id (so `read` and `PERF_EVENT_IOC_ID` agree), not from this snapshot.
    pub read_format: u64,
    /// Samples the ring dropped for this event (`PERF_FORMAT_LOST`). `0` for
    /// counting-only events (no sampling ring).
    pub lost: u64,
}

/// File-like handle returned by `perf_event_open(2)`. Locks a
/// `Box<dyn PerfEventOps>` so the inner implementation can stay generic.
pub struct PerfEvent {
    event: SpinNoPreempt<Box<dyn PerfEventOps>>,
    /// Unique, stable perf-event id (see [`NEXT_PERF_EVENT_ID`]). Returned by
    /// `PERF_EVENT_IOC_ID` and used as the `read_format` `PERF_FORMAT_ID` value.
    id: u64,
    /// O_NONBLOCK flag set via `fcntl(F_SETFL)`. When true, operations that
    /// would block (e.g. reading from an empty ring buffer) should return
    /// `EAGAIN` instead.
    nonblocking: AtomicBool,
    /// Group members led by THIS event: events opened with `group_fd` set to this
    /// event's fd (`perf stat -e '{a,b}'`). Empty for a solo event or a member.
    /// The leader drives the group — `ioctl(ENABLE/DISABLE)` propagates to members
    /// (Linux group scheduling), and a leader read with `PERF_FORMAT_GROUP`
    /// returns every member's counter in one buffer. `Weak` so a member closing
    /// its fd is not kept alive here; dead members are reaped on the next
    /// enable/disable/read.
    members: SpinNoPreempt<Vec<Weak<PerfEvent>>>,
}

impl Debug for PerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfEvent").field("id", &self.id).finish()
    }
}

impl PerfEvent {
    /// Wrap a per-type perf event impl, assigning it a fresh unique id and
    /// threading that id into the inner event so its samples carry it.
    pub fn new(mut event: Box<dyn PerfEventOps>) -> Self {
        let id = NEXT_PERF_EVENT_ID.fetch_add(1, Ordering::Relaxed);
        event.set_sample_id(id);
        PerfEvent {
            event: SpinNoPreempt::new(event),
            id,
            nonblocking: AtomicBool::new(false),
            members: SpinNoPreempt::new(Vec::new()),
        }
    }

    /// Borrow the inner impl under the lock.
    pub fn event(&self) -> SpinNoPreemptGuard<'_, Box<dyn PerfEventOps>> {
        self.event.lock()
    }

    /// Handle `PERF_EVENT_IOC_SET_OUTPUT`: redirect this event's records into the
    /// ring owned by the perf event whose fd is `arg` (or detach when `arg == -1`).
    ///
    /// `perf record` opens its events on one CPU/task and points all but the
    /// leader at the leader's single mmap ring with this ioctl. The redirect is a
    /// real merge: a hardware sampling source ([`hw::HwPerfEvent`]) starts writing
    /// its overflow `PERF_RECORD_SAMPLE`s into the target's ring (so `perf record
    /// -e a,b` captures both events). Sources that produce no ring records (the
    /// `PERF_COUNT_SW_DUMMY` tracking event, tracing variants) accept as a no-op.
    fn set_output(&self, arg: usize) -> AxResult<usize> {
        // `arg == -1` detaches the output (Linux semantics); nothing to wire.
        if arg as i32 == -1 {
            return Ok(0);
        }
        // The target must be an open perf-event fd, else EINVAL (Linux behaviour
        // for a non-perf or bad output fd).
        let target = get_file_like(arg as i32)?;
        let target = target
            .into_any_arc()
            .downcast::<PerfEvent>()
            .map_err(|_| AxError::InvalidInput)?;
        // Pull the target's ring (a mapped HW sampling event) and point this
        // event's output at it. If the target has no ring (e.g. it is itself a
        // non-mmap'd or non-sampling event), there is nothing to merge into; the
        // source keeps its own ring — `redirect_output` is then never called.
        if let Some((ring_vaddr, ring_len, anchor)) = target.event.lock().output_ring() {
            self.event
                .lock()
                .redirect_output(ring_vaddr, ring_len, anchor)?;
        }
        Ok(0)
    }

    /// Enable (`on`) or disable every live group member, reaping any that have
    /// closed. Called after the leader's own enable/disable so the group is
    /// scheduled as a unit (Linux group semantics: members follow the leader).
    fn propagate_members(&self, on: bool) {
        self.members.lock().retain(|w| {
            let Some(m) = w.upgrade() else {
                return false; // reap a closed member
            };
            let mut ev = m.event.lock();
            let _ = if on { ev.enable() } else { ev.disable() };
            true
        });
    }

    /// Serialize a `PERF_FORMAT_GROUP` read: the whole group's counters in one
    /// buffer — `nr`, then `time_enabled`/`time_running` once (from the leader),
    /// then per event (leader first, then live members) `value` plus `id` /
    /// `lost` when requested. `leader` is the already-read leader snapshot.
    fn read_group(&self, dst: &mut crate::file::IoDst, leader: &PerfReadValues) -> AxResult<usize> {
        let rf = leader.read_format;
        let live: Vec<Arc<PerfEvent>> = self
            .members
            .lock()
            .iter()
            .filter_map(|w| w.upgrade())
            .collect();

        let mut out: Vec<u64> = Vec::with_capacity(3 + 3 * (1 + live.len()));
        out.push(1 + live.len() as u64); // nr
        if rf & PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
            out.push(leader.time_enabled);
        }
        if rf & PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
            out.push(leader.time_running);
        }
        // Leader entry, then each member's.
        out.push(leader.value);
        if rf & PERF_FORMAT_ID != 0 {
            out.push(self.id);
        }
        if rf & PERF_FORMAT_LOST != 0 {
            out.push(leader.lost);
        }
        for m in &live {
            let mv = m.event.lock().read_values()?;
            out.push(mv.value);
            if rf & PERF_FORMAT_ID != 0 {
                out.push(m.id);
            }
            if rf & PERF_FORMAT_LOST != 0 {
                out.push(mv.lost);
            }
        }

        let total = out.len() * core::mem::size_of::<u64>();
        if dst.remaining_mut() < total {
            return Err(AxError::InvalidInput);
        }
        for v in &out {
            dst.write(&v.to_ne_bytes())?;
        }
        Ok(total)
    }
}

impl Pollable for PerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        self.event.lock().poll()
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: axpoll::IoEvents) {
        self.event.lock().register(context, events)
    }
}

impl FileLike for PerfEvent {
    fn read(&self, dst: &mut crate::file::IoDst) -> AxResult<usize> {
        // A hardware-PMU event reads as a sequence of native-endian `u64`s in
        // Linux's strict `read_format` order: always `value`; then
        // `time_enabled` if `PERF_FORMAT_TOTAL_TIME_ENABLED`; then
        // `time_running` if `PERF_FORMAT_TOTAL_TIME_RUNNING`; then `id` if
        // `PERF_FORMAT_ID`. `PERF_FORMAT_GROUP` is unsupported in M1. With
        // `read_format == 0` this is exactly the 8-byte bare counter value
        // (M0 behaviour). The tracing variants keep the default `read_values`
        // and propagate `Unsupported` here.
        let values = self.event.lock().read_values()?;

        // Group-leader read (`PERF_FORMAT_GROUP`): return the whole group's
        // counters in one buffer instead of the flat single-event layout.
        if values.read_format & PERF_FORMAT_GROUP != 0 {
            return self.read_group(dst, &values);
        }

        // Build the field sequence gated by `read_format`, in Linux order.
        let mut fields = [0u64; 5];
        let mut n = 0;
        fields[n] = values.value;
        n += 1;
        if values.read_format & PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
            fields[n] = values.time_enabled;
            n += 1;
        }
        if values.read_format & PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
            fields[n] = values.time_running;
            n += 1;
        }
        if values.read_format & PERF_FORMAT_ID != 0 {
            // The id is the wrapper's, so `read(perf_fd)` reports the same value
            // `PERF_EVENT_IOC_ID` handed userspace (the inner snapshot has none).
            fields[n] = self.id;
            n += 1;
        }
        if values.read_format & PERF_FORMAT_LOST != 0 {
            fields[n] = values.lost;
            n += 1;
        }

        let total = n * core::mem::size_of::<u64>();
        if dst.remaining_mut() < total {
            return Err(AxError::InvalidInput);
        }
        for value in &fields[..n] {
            dst.write(&value.to_ne_bytes())?;
        }
        Ok(total)
    }

    fn write(&self, _src: &mut crate::file::IoSrc) -> AxResult<usize> {
        Err(AxError::Unsupported)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[perf_event]".into()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        // Several perf ioctls carry a `_IOC` direction/size in the high bits
        // (`PERF_EVENT_IOC_ID` is `_IOR`, `SET_OUTPUT` is `_IO`), so match on the
        // `('$', nr)` pair rather than the full encoded value. These are absent
        // from `kbpf_basic`'s `PerfEventIoc`, so handle them before the enum
        // conversion (which would otherwise reject them as `EINVAL`).
        if (cmd >> 8) & 0xff == PERF_IOC_TYPE {
            match cmd & 0xff {
                // `PERF_EVENT_IOC_ID`: write this event's unique id (a `u64`) to
                // the user pointer in `arg`. `perf record` issues this right after
                // `mmap` to build its id→event map; rejecting it makes perf abort
                // with the misleading "failed to mmap" error.
                PERF_IOC_NR_ID => {
                    VmBytesMut::new(arg as *mut u8, core::mem::size_of::<u64>())
                        .write(&self.id.to_ne_bytes())?;
                    return Ok(0);
                }
                // `PERF_EVENT_IOC_SET_OUTPUT`: redirect this event's records into
                // the ring buffer owned by the perf event whose fd is `arg`
                // (or detach when `arg == -1`). `perf record` uses this so the
                // events it opens on one CPU/task share a single mmap ring.
                PERF_IOC_NR_SET_OUTPUT => {
                    return self.set_output(arg);
                }
                _ => {}
            }
        }
        // `PERF_EVENT_IOC_RESET` (0x2403) is absent from `kbpf_basic`'s
        // `PerfEventIoc`, so handle it before the enum conversion. Only the
        // hardware-PMU variant implements `reset`; the tracing variants keep
        // the default and return `Unsupported`.
        const PERF_EVENT_IOC_RESET: u32 = 0x2403;
        if cmd == PERF_EVENT_IOC_RESET {
            self.event.lock().reset()?;
            return Ok(0);
        }
        let req = PerfEventIoc::try_from(cmd).map_err(|_| AxError::InvalidInput)?;
        match req {
            PerfEventIoc::Enable => {
                self.event.lock().enable()?;
                // A group leader enable starts the whole group (members follow).
                self.propagate_members(true);
            }
            PerfEventIoc::Disable => {
                self.event.lock().disable()?;
                self.propagate_members(false);
            }
            PerfEventIoc::SetBpf => {
                let bpf_prog_fd = arg as i32;
                let file = get_file_like(bpf_prog_fd)?;
                self.event.lock().set_bpf_prog(file)?;
            }
        }
        Ok(0)
    }

    fn device_mmap(&self, offset: u64, length: u64) -> AxResult<DeviceMmap> {
        // libbpf calls mmap with offset == 0; non-zero offsets address into
        // the ringbuf, which has no meaningful sub-region exposed as a fd
        // offset (data_offset lives inside the header page).
        if offset != 0 {
            return Err(AxError::InvalidInput);
        }
        let len = length as usize;
        let (paddr, anchor) = self.event.lock().device_mmap(len)?;
        // Anchor the ringbuf pages to the VMA: the retainer keeps them alive
        // until `munmap`/exit, so closing the perf fd can't free memory the
        // user address space still maps. See `BpfPerfEventWrapper::pages`.
        //
        // CACHEABLE, not `Physical`/`UNCACHED`: these are RAM pages the kernel
        // writes through its cacheable linear map (the mmap-page header, the
        // sample ring, the rdpmc counter page) and userspace reads back. Both
        // are Normal Inner-Shareable cacheable mappings of the same physical
        // page, so the hardware keeps them coherent with no explicit
        // maintenance. An `UNCACHED` user mapping reads stale zeros on real
        // silicon (the kernel's cached writes never reach RAM) — a bug QEMU
        // hides because it models no caches.
        Ok(DeviceMmap::PhysicalCached(
            PhysAddrRange::from_start_size(paddr, len),
            Some(anchor),
        ))
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, on: bool) -> AxResult {
        self.nonblocking.store(on, Ordering::Release);
        Ok(())
    }
}

/// `perf_event_open(2)` syscall entry. Copies the user `perf_event_attr` in
/// and trampolines into [`perf_event_open`], which holds the dispatcher
/// across kprobe / tracepoint / software / uprobe / hardware types.
pub fn sys_perf_event_open(
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    let mut buf = vec![0u8; core::mem::size_of::<perf_event_attr>()];
    VmBytes::new(attr_uptr as *mut u8, buf.len()).read(&mut buf)?;
    // SAFETY: perf_event_attr is a `repr(C)` POD; the user buffer is copied
    // bytewise above and we treat the result as the structure.
    let attr = unsafe { &*(buf.as_ptr() as *const perf_event_attr) };
    perf_event_open(attr, pid, cpu, group_fd, flags as u32)
}

/// Dispatcher entry point for `perf_event_open(2)`. Reads the user-supplied
/// `perf_event_attr`, selects the per-type implementation, registers a
/// file-like in the current fd table and remembers a weak handle so the
/// ringbuf output path can locate the event by fd later.
pub fn perf_event_open(
    attr: &perf_event_attr,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u32,
) -> AxResult<isize> {
    // Hardware-PMU events (`PERF_TYPE_HARDWARE` / `PERF_TYPE_RAW`, plus the
    // dynamic ARM PMUv3 type `hw::ARMV8_PMUV3_PERF_TYPE` the real `perf` tool
    // resolves from sysfs) must be dispatched before
    // `PerfProbeArgs::try_from_perf_attr`, which maps any non-probe type through
    // `perf_sw_ids` and rejects hardware configs with `EINVAL`.
    let event: Box<dyn PerfEventOps> = if attr.type_ == PerfTypeId::PERF_TYPE_HARDWARE as u32
        || attr.type_ == PerfTypeId::PERF_TYPE_HW_CACHE as u32
        || attr.type_ == PerfTypeId::PERF_TYPE_RAW as u32
        || attr.type_ == hw::ARMV8_PMUV3_PERF_TYPE
        || attr.type_ == hw::ARMV8_CORTEX_A55_TYPE
        || attr.type_ == hw::ARMV8_CORTEX_A76_TYPE
    {
        // Thread `pid` + `cpu` into the hardware path: it chooses between
        // per-task counting (`pid > 0`), a cpu-bound system-wide event
        // (`pid <= 0 && cpu >= 0`, the `perf stat -a` fan-out — counts on that
        // core via its per-CPU pool), and the current-core path (`cpu < 0`).
        // `group_fd` / `flags` are not consumed by the hardware path.
        Box::new(hw::perf_event_open_hw(attr, pid, cpu)?)
    } else {
        let args = PerfProbeArgs::try_from_perf_attr::<EbpfKernelAuxiliary>(
            attr, pid, cpu, group_fd, flags,
        )
        .into_ax_result()?;
        match args.type_ {
            PerfTypeId::PERF_TYPE_KPROBE => Box::new(kprobe::perf_event_open_kprobe(args)?),
            // The five counting software events (`perf stat`'s default rows) become
            // real per-task counters; every other software config (e.g.
            // `PERF_COUNT_SW_DUMMY`, `perf record`'s tracking event) keeps the
            // BPF/ring path.
            PerfTypeId::PERF_TYPE_SOFTWARE => match &args.config {
                PerfProbeConfig::PerfSwIds(sw_id) if sw::is_counting_sw(*sw_id) => {
                    Box::new(sw::perf_event_open_sw(attr, *sw_id, pid)?)
                }
                _ => Box::new(bpf::perf_event_open_bpf(args)),
            },
            PerfTypeId::PERF_TYPE_TRACEPOINT => {
                Box::new(tracepoint::perf_event_open_tracepoint(args)?)
            }
            PerfTypeId::PERF_TYPE_UPROBE => Box::new(uprobe::perf_event_open_uprobe(args)?),
            _ => {
                warn!("perf_event_open: unsupported type {:?}", args.type_);
                return Err(AxError::Unsupported);
            }
        }
    };
    let perf_event = Arc::new(PerfEvent::new(event));
    // Group membership: a non-negative `group_fd` makes this event a member of the
    // group led by that fd (`perf stat -e '{a,b}'`). The member follows the
    // leader's enable state, so open it disabled until the leader's `ioctl(ENABLE)`
    // starts the whole group; the leader records it (weakly) for group
    // enable/disable propagation and `PERF_FORMAT_GROUP` reads.
    if group_fd >= 0 {
        let leader = get_file_like(group_fd)?
            .into_any_arc()
            .downcast::<PerfEvent>()
            .map_err(|_| AxError::InvalidInput)?;
        perf_event.event.lock().disable().ok();
        leader.members.lock().push(Arc::downgrade(&perf_event));
    }
    let event_arc: Arc<dyn FileLike> = perf_event;
    // Honour PERF_FLAG_FD_CLOEXEC: Linux opens the perf fd with O_CLOEXEC when
    // the caller sets this flag, otherwise the fd survives execve.
    let cloexec = flags & PERF_FLAG_FD_CLOEXEC != 0;
    let fd = add_file_like(event_arc.clone(), cloexec)?;

    PERF_FILE
        .get()
        .expect("perf subsystem not initialized")
        .lock()
        .insert(fd as usize, Arc::downgrade(&event_arc));

    Ok(fd as isize)
}

/// Map fd → weak<PerfEvent> so `bpf_perf_event_output` can locate the
/// target ringbuf without owning a strong reference (the user side owns
/// it via the fd).
static PERF_FILE: LazyInit<SpinNoPreempt<HashMap<usize, alloc::sync::Weak<dyn FileLike>>>> =
    LazyInit::new();

/// Initialize the perf-event runtime: build the fd→event lookup table and
/// register the Tier-2 rotation tick with the periodic scheduler tick.
pub fn perf_event_init() {
    PERF_FILE.init_once(SpinNoPreempt::new(HashMap::new()));
    // Drive per-CPU counter rotation (multiplexing) from the scheduler tick.
    #[cfg(target_arch = "aarch64")]
    ax_task::set_perf_tick(tick::perf_tick);
}

/// Implementation of `bpf_perf_event_output` helper: walk the fd→event map,
/// downcast the strong upgrade to `PerfEvent`, and have the bpf-software
/// variant write a record into the ringbuf.
pub fn perf_event_output(_ctx: *mut c_void, fd: usize, _flags: u32, data: &[u8]) -> AxResult<()> {
    let table = PERF_FILE.get().ok_or(AxError::NotFound)?;
    let mut map = table.lock();
    let weak = map.get(&fd).ok_or(AxError::NotFound)?;
    let Some(file) = weak.upgrade() else {
        map.remove(&fd);
        return Err(AxError::NotFound);
    };
    drop(map);

    let perf_event = file
        .into_any_arc()
        .downcast::<PerfEvent>()
        .map_err(|_| AxError::InvalidInput)?;
    let mut inner = perf_event.event();
    let bpf_event = inner
        .as_any_mut()
        .downcast_mut::<BpfPerfEventWrapper>()
        .ok_or(AxError::InvalidInput)?;
    bpf_event.write_event(data)?;
    Ok(())
}

/// Executable kernel mapping used by rbpf JIT programs on x86_64.
#[allow(unused)]
struct BPFJitMemory {
    num_pages: usize,
    pages: VirtAddr,
}

#[allow(unused)]
impl BPFJitMemory {
    fn new(num_pages: usize) -> AxResult<Self> {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        let virt_start = guard
            .find_free_area(
                guard.base(),
                num_pages * PAGE_SIZE_4K,
                VirtAddrRange::new(guard.base(), guard.end()),
            )
            .ok_or(AxError::NoMemory)?;
        guard.map_alloc(
            virt_start,
            num_pages * PAGE_SIZE_4K,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
            true,
        )?;

        Ok(BPFJitMemory {
            num_pages,
            pages: virt_start,
        })
    }

    /// Returns a `'static` mutable slice for rbpf's JIT memory registration.
    ///
    /// SAFETY: the caller must keep `self` alive and exclusively owned for at
    /// least as long as the returned slice may be used. The slice must not be
    /// used after this `BPFJitMemory` is dropped, because drop unmaps the
    /// backing pages.
    unsafe fn as_static_mut_slice(&mut self) -> &'static mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.pages.as_ptr() as *mut u8,
                self.num_pages * PAGE_SIZE_4K,
            )
        }
    }
}

impl Drop for BPFJitMemory {
    fn drop(&mut self) {
        let kspace = ax_mm::kernel_aspace();
        let mut guard = kspace.lock();
        guard
            .unmap(self.pages, self.num_pages * PAGE_SIZE_4K)
            .expect("failed to unmap BPF JIT memory");
    }
}
