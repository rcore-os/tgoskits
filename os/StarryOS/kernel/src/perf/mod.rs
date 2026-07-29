//! `perf_event_open(2)` runtime: dispatcher across kprobe / tracepoint /
//! software-bpf / uprobe perf event types, the file-like `PerfEvent`
//! wrapper, and the ringbuf output path used by the `bpf_perf_event_output`
//! helper. The `mmap(perf_fd, ...)` path is wired through
//! `FileLike::device_mmap` → `PerfEventOps::device_mmap`, which allocates
//! the backing pages and asks `kbpf_basic` to initialize the
//! `perf_event_mmap_page` header.

mod access;
mod access_policy;
pub mod bpf;
mod control;
#[cfg(target_arch = "aarch64")]
mod cpu_worker;
pub mod hw;
#[cfg(target_arch = "aarch64")]
mod hw_allocation;
mod hw_event;
#[cfg(target_arch = "aarch64")]
mod hw_open;
#[cfg(target_arch = "aarch64")]
mod hw_owner;
#[cfg(target_arch = "aarch64")]
mod hw_sampling;
#[cfg(target_arch = "aarch64")]
mod inheritance;
#[cfg(target_arch = "aarch64")]
mod inheritance_lifecycle;
pub mod kprobe;
#[cfg(target_arch = "aarch64")]
mod output;
pub mod raw_tracepoint;
#[cfg(target_arch = "aarch64")]
mod rdpmc;
#[cfg(target_arch = "aarch64")]
mod resource_lifecycle;
/// PMU overflow-IRQ sampling backend (M2). ARM PMUv3 only; the counting and
/// tracing paths are arch-agnostic, but sampling depends on CPU PMU registers.
#[cfg(target_arch = "aarch64")]
pub mod sampling;
#[cfg(target_arch = "aarch64")]
mod sampling_lifecycle;
#[cfg(target_arch = "aarch64")]
mod sampling_registry;
/// Side-band records (`PERF_RECORD_COMM`/`MMAP2`/`FORK`/`EXIT`) for `perf report`
/// symbolization. Writes into the sampling ring from process context, so it is
/// gated like `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod sideband;
mod target;
/// Per-task hardware-PMU counting (`perf stat -- cmd`, M3). ARM PMUv3 only; the
/// scheduler hooks call into CPU PMU register helpers, so it is gated like
/// `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod task;
#[cfg(target_arch = "aarch64")]
mod task_sideband;
pub mod tracepoint;
pub mod uprobe;

use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec};
use core::{
    any::Any,
    ffi::c_void,
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_io::{Read, Write};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{paging::MappingFlags, pmu};
use ax_sync::Mutex;
use axpoll::Pollable;
pub use bpf::BpfPerfEventWrapper;
use hashbrown::HashMap;
use kbpf_basic::{
    linux_bpf::perf_event_attr,
    perf::{PerfEventIoc, PerfProbeArgs, PerfTypeId},
};

#[cfg(target_arch = "aarch64")]
use self::output::validate_output_redirect;
use self::{
    access::ResolvedPerfTarget,
    control::PerfControl,
    target::{PerfOpenFlags, PerfTarget, PerfTargetError},
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
    /// Completes post-id initialization before the fd is published.
    fn finish_open(&mut self) -> AxResult<()> {
        Ok(())
    }

    /// Begin firing into the registered BPF program / ringbuf.
    fn enable(&mut self) -> AxResult<()>;

    /// Stop firing without tearing down the event.
    fn disable(&mut self) -> AxResult<()>;

    /// `Any` upcast (mutable). Used while constructing [`PerfEvent`] to recover
    /// capabilities exposed by concrete implementations.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Attach a BPF program to this event (`PERF_EVENT_IOC_SET_BPF`).
    fn set_bpf_prog(&mut self, _bpf_prog: Arc<dyn FileLike>) -> AxResult<()> {
        Err(AxError::Unsupported)
    }

    /// Allocate the user-visible ringbuf and return its physical start
    /// address (length is the user-supplied mmap length, page-aligned)
    /// together with a retainer that owns the backing pages. The caller
    /// threads the retainer into `DeviceMmap::Physical(.., Some(anchor))`
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
}

/// `read_format` bit selecting `time_enabled` in `read(perf_fd)`.
const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
/// `read_format` bit selecting `time_running` in `read(perf_fd)`.
const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
/// `read_format` bit selecting the per-event `id` in `read(perf_fd)`.
const PERF_FORMAT_ID: u64 = 1 << 2;

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
}

/// File-like handle returned by `perf_event_open(2)`.
///
/// Task-context control operations use a blocking mutex because callbacks may
/// allocate, fault, or reschedule. Software BPF output has a separate
/// non-sleeping capability containing only the bounded ring-write state needed
/// by trace and IRQ producers.
pub struct PerfEvent {
    event: Mutex<Box<dyn PerfEventOps>>,
    /// Bounded non-sleeping output endpoint for software BPF events.
    irq_output: Option<bpf::BpfPerfOutput>,
    /// Sleepable control plane, kept separate from IRQ/BPF output access.
    control: Option<Arc<dyn PerfControl>>,
    /// Unique, stable perf-event id (see [`NEXT_PERF_EVENT_ID`]). Returned by
    /// `PERF_EVENT_IOC_ID` and used as the `read_format` `PERF_FORMAT_ID` value.
    id: u64,
    /// O_NONBLOCK flag set via `fcntl(F_SETFL)`. When true, operations that
    /// would block (e.g. reading from an empty ring buffer) should return
    /// `EAGAIN` instead.
    nonblocking: AtomicBool,
}

impl Debug for PerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfEvent").field("id", &self.id).finish()
    }
}

impl PerfEvent {
    /// Wrap a per-type perf event impl, assigning it a fresh unique id and
    /// threading that id into the inner event so its samples carry it.
    pub fn new(mut event: Box<dyn PerfEventOps>) -> AxResult<Self> {
        let id = NEXT_PERF_EVENT_ID.fetch_add(1, Ordering::Relaxed);
        event.set_sample_id(id);
        event.finish_open()?;
        let irq_output = event
            .as_any_mut()
            .downcast_mut::<BpfPerfEventWrapper>()
            .map(|event| event.output_handle());
        #[cfg(target_arch = "aarch64")]
        let control = event
            .as_any_mut()
            .downcast_mut::<hw::HwPerfEvent>()
            .map(|event| event.control_handle());
        #[cfg(not(target_arch = "aarch64"))]
        let control = None;
        Ok(PerfEvent {
            event: Mutex::new(event),
            irq_output,
            control,
            id,
            nonblocking: AtomicBool::new(false),
        })
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
        // `arg == -1` detaches the output and returns to the event's own ring.
        if arg as i32 == -1 {
            #[cfg(target_arch = "aarch64")]
            if let Some(control) = &self.control {
                control.detach_output()?;
            }
            return Ok(0);
        }
        // The target must be an open perf-event fd, else EINVAL (Linux behaviour
        // for a non-perf or bad output fd).
        let target = get_file_like(arg as i32)?;
        let target = target
            .into_any_arc()
            .downcast::<PerfEvent>()
            .map_err(|_| AxError::InvalidInput)?;
        if target.id == self.id {
            return Err(AxError::InvalidInput);
        }
        #[cfg(not(target_arch = "aarch64"))]
        return Err(AxError::InvalidInput);

        #[cfg(target_arch = "aarch64")]
        {
            let target_control = target.control.as_ref().ok_or(AxError::InvalidInput)?;
            let target_scope = target_control.output_scope().ok_or(AxError::InvalidInput)?;
            let output = target_control.output_ring().ok_or(AxError::InvalidInput)?;

            // Events without a hardware output producer (for example the software
            // dummy tracking event) accept the Linux ioctl as a no-op after the
            // target fd/ring has been validated.
            if let Some(control) = &self.control {
                let source_scope = control.output_scope().ok_or(AxError::InvalidInput)?;
                validate_output_redirect(self.id, target.id, source_scope, target_scope)
                    .map_err(|_| AxError::InvalidInput)?;
                control.redirect_output(output)?;
            }
            Ok(0)
        }
    }
}

impl Pollable for PerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        if let Some(control) = &self.control {
            control.poll()
        } else {
            self.event.lock().poll()
        }
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: axpoll::IoEvents) {
        if let Some(control) = &self.control {
            control.register(context, events);
        } else {
            self.event.lock().register(context, events);
        }
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
        let values = if let Some(control) = &self.control {
            control.read_values()?
        } else {
            self.event.lock().read_values()?
        };

        // Build the field sequence gated by `read_format`, in Linux order.
        let mut fields = [0u64; 4];
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
            if let Some(control) = &self.control {
                control.reset()?;
            } else {
                self.event.lock().reset()?;
            }
            return Ok(0);
        }
        let req = PerfEventIoc::try_from(cmd).map_err(|_| AxError::InvalidInput)?;
        match req {
            PerfEventIoc::Enable => {
                if let Some(control) = &self.control {
                    control.enable()?;
                } else {
                    self.event.lock().enable()?;
                }
            }
            PerfEventIoc::Disable => {
                if let Some(control) = &self.control {
                    control.disable()?;
                } else {
                    self.event.lock().disable()?;
                }
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
        let (paddr, anchor) = if let Some(control) = &self.control {
            control.device_mmap(len)?
        } else {
            self.event.lock().device_mmap(len)?
        };
        // Anchor the ringbuf pages to the VMA: the retainer keeps them alive
        // until `munmap`/exit, so closing the perf fd can't free memory the
        // user address space still maps. See `BpfPerfEventWrapper::pages`.
        Ok(DeviceMmap::Physical(
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
    perf_event_open(attr, pid, cpu, group_fd, flags)
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
    flags: u64,
) -> AxResult<isize> {
    let flags = PerfOpenFlags::parse(flags).map_err(|_| AxError::InvalidInput)?;
    if flags.contains(PerfOpenFlags::PID_CGROUP) {
        return Err(AxError::Unsupported);
    }
    let target = PerfTarget::parse(pid, cpu).map_err(|error| match error {
        PerfTargetError::InvalidTuple => AxError::InvalidInput,
        PerfTargetError::NoSuchProcess => AxError::NoSuchProcess,
    })?;
    let target = ResolvedPerfTarget::resolve(target, ax_runtime::hal::cpu_num())?;

    // Starry does not yet deliver synchronous perf SIGTRAP notifications.
    // Reject the capability explicitly instead of accepting an event whose
    // signal side effect would be silently missing. The access policy still
    // models Linux's CAP_KILL rule so enabling the feature cannot bypass it.
    if attr.sigtrap() != 0 {
        return Err(AxError::Unsupported);
    }

    let is_hardware = attr.type_ == PerfTypeId::PERF_TYPE_HARDWARE as u32
        || attr.type_ == PerfTypeId::PERF_TYPE_RAW as u32
        || attr.type_ == hw::ARMV8_PMUV3_PERF_TYPE;
    let validated_hw = is_hardware
        .then(|| hw::validate_perf_event_open_hw(attr, target.kind()))
        .transpose()?;
    let probe_args = if is_hardware {
        None
    } else {
        Some(
            PerfProbeArgs::try_from_perf_attr::<EbpfKernelAuxiliary>(
                attr,
                pid,
                cpu,
                group_fd,
                flags.bits(),
            )
            .into_ax_result()?,
        )
    };

    target.with_authorized(attr.sigtrap() != 0, |target| {
        // Hardware-PMU events (`PERF_TYPE_HARDWARE` / `PERF_TYPE_RAW`, plus
        // the dynamic ARM PMUv3 type `hw::ARMV8_PMUV3_PERF_TYPE`) bypass
        // `PerfProbeArgs`, which maps non-probe configs through `perf_sw_ids`.
        let event: Box<dyn PerfEventOps> = if is_hardware {
            Box::new(hw::perf_event_open_hw(
                attr,
                target,
                validated_hw.expect("hardware perf open has validated attributes"),
            )?)
        } else {
            let args = probe_args.expect("non-hardware perf open has validated probe arguments");
            match args.type_ {
                PerfTypeId::PERF_TYPE_KPROBE => Box::new(kprobe::perf_event_open_kprobe(args)?),
                PerfTypeId::PERF_TYPE_SOFTWARE => Box::new(bpf::perf_event_open_bpf(args)),
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
        let event_arc: Arc<dyn FileLike> = Arc::new(PerfEvent::new(event)?);
        // Honour PERF_FLAG_FD_CLOEXEC: Linux opens the perf fd with O_CLOEXEC
        // when the caller sets this flag, otherwise the fd survives execve.
        let cloexec = flags.contains(PerfOpenFlags::FD_CLOEXEC);
        let fd = add_file_like(event_arc.clone(), cloexec)?;

        PERF_FILE
            .get()
            .expect("perf subsystem not initialized")
            .lock()
            .insert(fd as usize, Arc::downgrade(&event_arc));

        Ok(fd as isize)
    })
}

/// Map fd → weak<PerfEvent> so `bpf_perf_event_output` can locate the
/// target ringbuf without owning a strong reference (the user side owns
/// it via the fd).
static PERF_FILE: LazyInit<SpinNoIrq<HashMap<usize, alloc::sync::Weak<dyn FileLike>>>> =
    LazyInit::new();

/// Initialize the perf-event runtime: build the fd→event lookup table.
pub fn perf_event_init() {
    PERF_FILE.init_once(SpinNoIrq::new(HashMap::new()));
    #[cfg(target_arch = "aarch64")]
    cpu_worker::init();
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
    perf_event
        .irq_output
        .as_ref()
        .ok_or(AxError::InvalidInput)?
        .write_event(data)
}

#[cfg(axtest)]
pub(crate) fn control_callback_runs_preemptible_for_test() -> bool {
    #[derive(Debug)]
    struct YieldingControl;

    impl Pollable for YieldingControl {
        fn poll(&self) -> axpoll::IoEvents {
            axpoll::IoEvents::empty()
        }

        fn register(&self, _context: &mut core::task::Context<'_>, _events: axpoll::IoEvents) {}
    }

    impl PerfEventOps for YieldingControl {
        fn enable(&mut self) -> AxResult<()> {
            ax_task::yield_now();
            Ok(())
        }

        fn disable(&mut self) -> AxResult<()> {
            Ok(())
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    let event = PerfEvent::new(Box::new(YieldingControl));
    event.ioctl(PerfEventIoc::Enable as u32, 0).is_ok()
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
