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
#[cfg(any(target_arch = "aarch64", test))]
mod counting;
mod cpu_id;
#[cfg(target_arch = "aarch64")]
mod cpu_worker;
#[cfg(target_arch = "aarch64")]
pub(crate) mod event_map;
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
mod nofault;
#[cfg(target_arch = "aarch64")]
mod output;
#[cfg(target_arch = "aarch64")]
pub mod percpu;
pub mod raw_tracepoint;
#[cfg(target_arch = "aarch64")]
mod rdpmc;
#[cfg(target_arch = "aarch64")]
mod resource_lifecycle;
#[cfg(target_arch = "aarch64")]
mod sample_id;
/// PMU overflow-IRQ sampling backend (M2). ARM PMUv3 only; the counting and
/// tracing paths are arch-agnostic, but sampling depends on CPU PMU registers.
#[cfg(target_arch = "aarch64")]
pub mod sampling;
#[cfg(any(target_arch = "aarch64", test))]
mod sampling_lifecycle;
#[cfg(target_arch = "aarch64")]
mod sampling_registry;
/// Side-band records (`PERF_RECORD_COMM`/`MMAP2`/`FORK`/`EXIT`) for `perf report`
/// symbolization. Writes into the sampling ring from process context, so it is
/// gated like `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod sideband;
/// Linux core `PERF_TYPE_SOFTWARE` counting events.
pub mod sw;
#[cfg(target_arch = "aarch64")]
mod system_flex;
mod target;
/// Per-task hardware-PMU counting (`perf stat -- cmd`, M3). ARM PMUv3 only; the
/// scheduler hooks call into CPU PMU register helpers, so it is gated like
/// `sampling`.
#[cfg(target_arch = "aarch64")]
pub mod task;
#[cfg(target_arch = "aarch64")]
pub(crate) mod task_context;
#[cfg(target_arch = "aarch64")]
mod task_context_state;
#[cfg(target_arch = "aarch64")]
mod task_sideband;
pub mod tracepoint;
pub mod uapi;
#[cfg(target_arch = "aarch64")]
mod unwind;
pub mod uprobe;

use alloc::{
    borrow::Cow,
    boxed::Box,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    ffi::c_void,
    fmt::Debug,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_io::Write;
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange, VirtAddr};
use ax_runtime::hal::paging::MappingFlags;
#[cfg(target_arch = "aarch64")]
use ax_runtime::hal::pmu;
use axpoll::{ExclusiveRegistrationSink, Pollable, SharedRegistrationSink};
pub use bpf::BpfPerfEventWrapper;
use hashbrown::HashMap;
use kbpf_basic::{
    linux_bpf::perf_event_attr,
    perf::{PerfEventIoc, PerfProbeArgs, PerfProbeConfig, PerfTypeId},
};

#[cfg(target_arch = "aarch64")]
use self::output::{PerfOutputScope, PerfRingOutput, validate_output_redirect};
use self::{
    access::ResolvedPerfTarget,
    control::PerfControl,
    target::{PerfContextKey, PerfTarget, PerfTargetError},
    uapi::{PerfOpenFlags, copy_perf_event_attr},
};
use crate::{
    StarryError, StarryResult,
    ebpf::{error::BpfResultExt, transform::EbpfKernelAuxiliary},
    file::{FileLike, Kstat, add_file_like, get_file_like},
    mm::VmBytesMut,
    pseudofs::DeviceMmap,
    sync::{IrqMutex, Mutex},
};

/// Monotonic source of per-event `perf` ids (`PERF_EVENT_IOC_ID`,
/// `PERF_SAMPLE_ID`, `read_format`'s `PERF_FORMAT_ID`). Linux assigns every
/// `perf_event` a unique non-zero id; `perf record` reads it back with
/// `PERF_EVENT_IOC_ID` right after `mmap` to build its id→event map, so the
/// value must be unique and stable for the life of the event. Starts at 1 so 0
/// stays reserved for "no id".
static NEXT_PERF_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Allocates a concrete event identity, including fd-less inherited events.
fn allocate_event_id() -> u64 {
    NEXT_PERF_EVENT_ID.fetch_add(1, Ordering::Relaxed)
}

/// `MIDR_EL1` for the cpuid `sysfs`/`procfs` nodes (`/proc/cpuinfo`,
/// `/sys/devices/.../cpuid`, `.../regs/identification/midr_el1`).
///
/// The real register on aarch64 (ARM PMUv3). The corresponding pseudo-fs node
/// is architecture-gated with this helper.
#[cfg(target_arch = "aarch64")]
pub fn read_midr_el1() -> u64 {
    pmu::cpu_id_raw().unwrap_or(0)
}

/// Cached `MIDR_EL1` for one logical CPU, populated by its fixed perf worker.
pub fn cpu_midr(cpu: usize) -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        percpu::cpu_info(cpu).map_or(0, |info| info.midr)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = cpu;
        0
    }
}

/// `ioctl` type byte for the perf-event ioctls (`'$'`).
const PERF_IOC_TYPE: u32 = 0x24;
/// `PERF_EVENT_IOC_SET_OUTPUT` request number (`_IO('$', 5)`).
const PERF_IOC_NR_SET_OUTPUT: u32 = 5;
/// `PERF_EVENT_IOC_ID` request number (`_IOR('$', 7, __u64 *)`).
const PERF_IOC_NR_ID: u32 = 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PerfGroupBackend {
    /// ARM PMU event.
    Hardware,
    /// Software counting event.
    Software,
    /// Probe, tracking, or another backend without a shared coordinator.
    Other,
}

/// Behaviour every perf event implements. Each variant in the dispatcher
/// (kprobe / tracepoint / software-bpf / uprobe / hardware-PMU) provides a
/// `Box<dyn PerfEventOps>` that `PerfEvent` then drives through the file
/// layer (`ioctl`, `mmap`, `read`, etc.).
pub trait PerfEventOps: Pollable + Send + Sync + Debug {
    /// Completes post-id initialization before the fd is published.
    fn finish_open(&mut self) -> crate::StarryResult<()> {
        Ok(())
    }

    /// Begin firing into the registered BPF program / ringbuf.
    fn enable(&mut self) -> StarryResult<()>;

    /// Stop firing without tearing down the event.
    fn disable(&mut self) -> StarryResult<()>;

    /// `Any` upcast (mutable). Used while constructing [`PerfEvent`] to recover
    /// capabilities exposed by concrete implementations.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Attach a BPF program to this event (`PERF_EVENT_IOC_SET_BPF`).
    fn set_bpf_prog(&mut self, _bpf_prog: Arc<dyn FileLike>) -> StarryResult<()> {
        Err(StarryError::Unsupported)
    }

    /// Allocate the user-visible ringbuf and return its physical start
    /// address (length is the user-supplied mmap length, page-aligned)
    /// together with a retainer that owns the backing pages. The caller
    /// threads the retainer into `DeviceMmap::Physical(.., Some(anchor))`
    /// so the pages stay live for as long as the user mapping exists, even
    /// after `close(perf_fd)`. Only `bpf::BpfPerfEventWrapper` overrides
    /// this; the other variants (kprobe/tracepoint/raw-tp/uprobe wrappers)
    /// reject `mmap(perf_fd)`.
    fn device_mmap(&mut self, _len: usize) -> StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        Err(StarryError::Unsupported)
    }

    /// Read the current counter value plus timing, for `read(perf_fd)`.
    ///
    /// Only the hardware-PMU variant ([`hw::HwPerfEvent`]) overrides this;
    /// the tracing variants have no counter to read and keep the default,
    /// so `read(perf_fd)` returns `Unsupported` for them. The returned
    /// [`PerfReadValues`] carries the raw counter value, the enabled/running
    /// times, and the `read_format` that [`PerfEvent::read`] uses to decide
    /// which of those fields to serialize.
    fn read_values(&mut self) -> StarryResult<PerfReadValues> {
        Err(StarryError::Unsupported)
    }

    /// Reset the counter to zero (`PERF_EVENT_IOC_RESET`).
    ///
    /// Only the hardware-PMU variant ([`hw::HwPerfEvent`]) overrides this;
    /// the tracing variants keep the default and reject the ioctl.
    fn reset(&mut self) -> StarryResult<()> {
        Err(StarryError::Unsupported)
    }

    /// Record the unique event id this event emits in its `PERF_SAMPLE_ID` /
    /// `PERF_SAMPLE_IDENTIFIER` sample fields. Called once by [`PerfEvent::new`]
    /// with the same id `PERF_EVENT_IOC_ID` reports, so a reader can demultiplex
    /// the events sharing one ring (`perf record -e a,b`). Default no-op: the
    /// tracing variants emit no hardware samples.
    fn set_sample_id(&mut self, _id: u64) {}

    /// Connects a backend to an already validated file-layer group leader.
    fn link_group(&mut self, _leader: &mut dyn PerfEventOps) -> StarryResult<()> {
        Ok(())
    }

    /// Whether this backend can participate in a file-layer event group.
    ///
    /// Backends override this when their implementation cannot provide the
    /// group control/read contract. The open path checks the leader before
    /// constructing a new backend so a rejected link has no PMU side effects.
    fn supports_group_link(&mut self) -> bool {
        true
    }

    /// Backend family used to reject combinations whose coordinator is not
    /// implemented. Returning success from `link_group` without linking the
    /// backend would publish a file-level group with unrelated schedulers.
    fn group_backend(&mut self) -> PerfGroupBackend {
        PerfGroupBackend::Other
    }

    /// Number of programmable PMU slots required by one pinned-group member.
    #[cfg(target_arch = "aarch64")]
    fn programmable_slots(&mut self) -> usize {
        0
    }

    #[cfg(target_arch = "aarch64")]
    fn output_scope(&mut self) -> Option<PerfOutputScope> {
        None
    }

    #[cfg(target_arch = "aarch64")]
    fn redirect_output(&mut self, _output: PerfRingOutput) -> StarryResult<()> {
        Err(StarryError::InvalidInput)
    }

    #[cfg(target_arch = "aarch64")]
    fn detach_output(&mut self) -> StarryResult<()> {
        Err(StarryError::InvalidInput)
    }

    /// Whether `PERF_EVENT_IOC_SET_OUTPUT` is an accepted no-op for a source
    /// that deliberately emits no records of its own.
    #[cfg(target_arch = "aarch64")]
    fn accepts_output_noop(&mut self) -> bool {
        false
    }
}

/// `read_format` bit selecting `time_enabled` in `read(perf_fd)`.
pub(crate) const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
/// `read_format` bit selecting `time_running` in `read(perf_fd)`.
pub(crate) const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
/// `read_format` bit selecting the per-event `id` in `read(perf_fd)`.
pub(crate) const PERF_FORMAT_ID: u64 = 1 << 2;
/// `read_format` bit selecting a leader-first group snapshot.
pub(crate) const PERF_FORMAT_GROUP: u64 = 1 << 3;
/// `read_format` bit selecting a per-event lost-sample count.
pub(crate) const PERF_FORMAT_LOST: u64 = 1 << 4;
const PERF_IOC_FLAG_GROUP: usize = 1;

/// Counter snapshot returned by [`PerfEventOps::read_values`].
///
/// Mirrors the fields Linux's `read(perf_fd)` can emit, gated by
/// `read_format`. M1 supports `value`, `time_enabled`, `time_running`, and
/// `id`; the file wrapper adds group serialization.
pub struct PerfReadValues {
    /// The raw counter value.
    pub value: u64,
    /// Wall time the event has been enabled, in nanoseconds.
    pub time_enabled: u64,
    /// Wall time the event was scheduled onto hardware, in nanoseconds.
    /// Equal to `time_enabled` in M1 (no multiplexing).
    pub time_running: u64,
    /// Samples dropped because the mmap ring had no free record space.
    pub lost: u64,
    /// `attr.read_format`, controlling which fields [`PerfEvent::read`] emits.
    /// The `PERF_FORMAT_ID` value itself comes from the owning [`PerfEvent`]'s
    /// id (so `read` and `PERF_EVENT_IOC_ID` agree), not from this snapshot.
    pub read_format: u64,
}

/// File-like handle returned by `perf_event_open(2)`.
///
/// Task-context control operations use a priority-inheriting sleepable mutex.
/// Software BPF output has a separate non-sleeping capability containing only
/// the bounded ring-write state needed by trace/IRQ producers.
pub struct PerfEvent {
    event: Mutex<Box<dyn PerfEventOps>>,
    /// Sleepable control plane, kept separate from IRQ/BPF output access.
    control: Option<Arc<dyn PerfControl>>,
    /// Bounded non-sleeping output endpoint for software BPF events.
    irq_output: Option<bpf::BpfPerfOutput>,
    /// Readiness endpoint that can register without holding the event mutex.
    bpf_poll: Option<bpf::BpfPerfPoll>,
    /// Unique, stable perf-event id (see [`NEXT_PERF_EVENT_ID`]). Returned by
    /// `PERF_EVENT_IOC_ID` and used as the `read_format` `PERF_FORMAT_ID` value.
    id: u64,
    /// O_NONBLOCK flag set via `fcntl(F_SETFL)`. When true, operations that
    /// would block (e.g. reading from an empty ring buffer) should return
    /// `EAGAIN` instead.
    nonblocking: AtomicBool,
    /// Generation-bearing task or fixed-CPU context used by group checks.
    context: Option<PerfContextKey>,
    /// `attr.inherit`, which Linux requires to agree inside a task group.
    inherit: bool,
    /// `attr.pinned` on this event (only a leader may carry it).
    #[cfg(target_arch = "aarch64")]
    pinned: bool,
    /// Pinned-group ERROR state. Linux exposes this as EOF from `read()`.
    group_error: AtomicBool,
    /// Live members owned weakly so closing fds cannot form a cycle.
    members: Mutex<Vec<Weak<PerfEvent>>>,
    /// Ordinary group leader, or `None` for a leader/standalone event.
    group_leader: Mutex<Option<Weak<PerfEvent>>>,
    /// Shared by a leader and its members, including after leader FD closure.
    transaction: Arc<Mutex<()>>,
}

impl Debug for PerfEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PerfEvent").field("id", &self.id).finish()
    }
}

impl PerfEvent {
    /// Wrap a per-type perf event impl, assigning it a fresh unique id and
    /// threading that id into the inner event so its samples carry it.
    pub fn new(
        mut event: Box<dyn PerfEventOps>,
        context: Option<PerfContextKey>,
        inherit: bool,
        pinned: bool,
    ) -> crate::StarryResult<Self> {
        let id = allocate_event_id();
        event.set_sample_id(id);
        event.finish_open()?;
        #[cfg(not(target_arch = "aarch64"))]
        let _ = pinned;
        #[cfg(target_arch = "aarch64")]
        let control = event
            .as_any_mut()
            .downcast_mut::<hw::HwPerfEvent>()
            .map(|event| event.control_handle());
        #[cfg(not(target_arch = "aarch64"))]
        let control = None;
        let irq_output = event
            .as_any_mut()
            .downcast_mut::<BpfPerfEventWrapper>()
            .map(|event| event.output_handle());
        let bpf_poll = event
            .as_any_mut()
            .downcast_mut::<BpfPerfEventWrapper>()
            .map(|event| event.poll_handle());
        Ok(PerfEvent {
            event: Mutex::new(event),
            control,
            irq_output,
            bpf_poll,
            id,
            nonblocking: AtomicBool::new(false),
            context,
            inherit,
            #[cfg(target_arch = "aarch64")]
            pinned,
            group_error: AtomicBool::new(false),
            members: Mutex::new(Vec::new()),
            group_leader: Mutex::new(None),
            transaction: Arc::new(Mutex::new(())),
        })
    }

    fn read_values(&self) -> StarryResult<PerfReadValues> {
        if let Some(control) = &self.control {
            control.read_values()
        } else {
            self.event.lock().read_values()
        }
    }

    fn set_enabled(&self, enabled: bool) -> StarryResult<()> {
        if let Some(control) = &self.control {
            if enabled {
                control.enable()
            } else {
                control.disable()
            }
        } else if enabled {
            self.event.lock().enable()
        } else {
            self.event.lock().disable()
        }
    }

    fn reset_one(&self) -> StarryResult<()> {
        if let Some(control) = &self.control {
            control.reset()
        } else {
            self.event.lock().reset()
        }
    }

    fn live_members(&self) -> Vec<Arc<PerfEvent>> {
        let mut members = self.members.lock();
        let live = members.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
        members.retain(|member| member.strong_count() != 0);
        live
    }

    fn propagate_members(&self, enable: bool) -> StarryResult<()> {
        let mut changed: Vec<Arc<PerfEvent>> = Vec::new();
        for member in self.live_members() {
            if let Err(error) = member.set_enabled(enable) {
                if enable {
                    for previous in changed {
                        let _ = previous.set_enabled(false);
                    }
                }
                return Err(error);
            }
            changed.push(member);
        }
        Ok(())
    }

    #[cfg(target_arch = "aarch64")]
    fn validate_pinned_group_capacity(&self) -> StarryResult<()> {
        if !self.pinned {
            self.group_error.store(false, Ordering::Release);
            return Ok(());
        }
        let Some(PerfContextKey::Cpu(cpu)) = self.context else {
            self.group_error.store(false, Ordering::Release);
            return Ok(());
        };
        let mut required = self.event.lock().programmable_slots();
        for member in self.live_members() {
            required += member.event.lock().programmable_slots();
        }
        let capacity = percpu::cpu_info(cpu.as_usize()).map_or(0, |info| info.num_counters);
        if required > capacity {
            self.group_error.store(true, Ordering::Release);
            return Err(StarryError::ResourceBusy);
        }
        self.group_error.store(false, Ordering::Release);
        Ok(())
    }

    fn reset_members(&self) -> StarryResult<()> {
        for member in self.live_members() {
            member.reset_one()?;
        }
        Ok(())
    }

    fn live_group_leader(&self) -> Option<Arc<Self>> {
        self.group_leader.lock().as_ref().and_then(Weak::upgrade)
    }

    fn control_group(&self, enable: Option<bool>) -> StarryResult<()> {
        self.control_group_observed(enable, || {})
    }

    fn control_group_observed(
        &self,
        enable: Option<bool>,
        before_leader_enable: impl FnOnce(),
    ) -> StarryResult<()> {
        // One transaction spans every member and the leader. Backend locks
        // remain inner locks and must not reacquire this sleepable boundary.
        let _transaction = self.transaction.lock();
        let leader = self.live_group_leader();
        let leader = leader.as_deref().unwrap_or(self);
        match enable {
            Some(true) => {
                #[cfg(target_arch = "aarch64")]
                leader.validate_pinned_group_capacity()?;
                leader.propagate_members(true)?;
                before_leader_enable();
                if let Err(error) = leader.set_enabled(true) {
                    let _ = leader.propagate_members(false);
                    return Err(error);
                }
                Ok(())
            }
            Some(false) => {
                leader.set_enabled(false)?;
                leader.propagate_members(false)
            }
            None => {
                leader.reset_one()?;
                leader.reset_members()
            }
        }
    }

    fn read_group(
        &self,
        dst: &mut crate::file::IoDst,
        leader: &PerfReadValues,
    ) -> StarryResult<usize> {
        if let Some(group_leader) = self.live_group_leader() {
            let mut values = group_leader.read_values()?;
            // Linux uses the addressed fd's read_format, even when the values
            // and ordering come from its group leader.
            values.read_format = leader.read_format;
            return group_leader.read_group(dst, &values);
        }
        let members = self.live_members();
        let mut fields = Vec::with_capacity(4 + members.len() * 2);
        fields.push(1 + members.len() as u64);
        if leader.read_format & PERF_FORMAT_TOTAL_TIME_ENABLED != 0 {
            fields.push(leader.time_enabled);
        }
        if leader.read_format & PERF_FORMAT_TOTAL_TIME_RUNNING != 0 {
            fields.push(leader.time_running);
        }
        fields.push(leader.value);
        if leader.read_format & PERF_FORMAT_ID != 0 {
            fields.push(self.id);
        }
        if leader.read_format & PERF_FORMAT_LOST != 0 {
            fields.push(leader.lost);
        }
        for member in members {
            let values = member.read_values()?;
            fields.push(values.value);
            if leader.read_format & PERF_FORMAT_ID != 0 {
                fields.push(member.id);
            }
            if leader.read_format & PERF_FORMAT_LOST != 0 {
                fields.push(values.lost);
            }
        }
        let total = fields.len() * core::mem::size_of::<u64>();
        if dst.remaining_mut() < total {
            return Err(StarryError::StorageFull);
        }
        for value in fields {
            dst.write(&value.to_ne_bytes())?;
        }
        Ok(total)
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
    fn set_output(&self, arg: usize) -> crate::StarryResult<usize> {
        // `arg == -1` detaches the output and returns to the event's own ring.
        if arg as i32 == -1 {
            #[cfg(target_arch = "aarch64")]
            if let Some(control) = &self.control {
                control.detach_output()?;
            } else {
                self.event.lock().detach_output()?;
            }
            return Ok(0);
        }
        // The target must be an open perf-event fd, else EINVAL (Linux behaviour
        // for a non-perf or bad output fd).
        let target = get_file_like(arg as i32)?;
        let target = target
            .into_any_arc()
            .downcast::<PerfEvent>()
            .map_err(|_| crate::StarryError::InvalidInput)?;
        self.set_output_target(&target)?;
        Ok(0)
    }

    fn set_output_target(&self, target: &PerfEvent) -> crate::StarryResult<()> {
        if target.id == self.id {
            return Err(crate::StarryError::InvalidInput);
        }
        #[cfg(not(target_arch = "aarch64"))]
        return Err(crate::StarryError::InvalidInput);

        #[cfg(target_arch = "aarch64")]
        {
            if self.context != target.context {
                return Err(crate::StarryError::InvalidInput);
            }
            let target_control = target
                .control
                .as_ref()
                .ok_or(crate::StarryError::InvalidInput)?;
            let target_scope = target_control
                .output_scope()
                .ok_or(crate::StarryError::InvalidInput)?;
            let output = target_control
                .output_ring()
                .ok_or(crate::StarryError::InvalidInput)?;

            if let Some(control) = &self.control {
                let source_scope = control
                    .output_scope()
                    .ok_or(crate::StarryError::InvalidInput)?;
                validate_output_redirect(self.id, target.id, source_scope, target_scope)
                    .map_err(|_| crate::StarryError::InvalidInput)?;
                control.redirect_output(output)?;
            } else {
                let mut source = self.event.lock();
                let Some(source_scope) = source.output_scope() else {
                    return source
                        .accepts_output_noop()
                        .then_some(())
                        .ok_or(crate::StarryError::InvalidInput);
                };
                validate_output_redirect(self.id, target.id, source_scope, target_scope)
                    .map_err(|_| crate::StarryError::InvalidInput)?;
                source.redirect_output(output)?;
            }
            Ok(())
        }
    }
}

impl Pollable for PerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        if let Some(control) = &self.control {
            control.poll()
        } else if let Some(poll) = &self.bpf_poll {
            poll.poll()
        } else {
            self.event.lock().poll()
        }
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn SharedRegistrationSink,
        events: axpoll::IoEvents,
    ) {
        if let Some(control) = &self.control {
            unsafe { control.register_shared(sink, events) };
        } else if let Some(poll) = &self.bpf_poll {
            unsafe { poll.register_shared(sink, events) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
        events: axpoll::IoEvents,
    ) {
        if let Some(control) = &self.control {
            unsafe { control.register_exclusive(sink, events) };
        } else if let Some(poll) = &self.bpf_poll {
            unsafe { poll.register_exclusive(sink, events) };
        }
    }
}

impl FileLike for PerfEvent {
    fn read(&self, dst: &mut crate::file::IoDst) -> StarryResult<usize> {
        let _transaction = self.transaction.lock();
        if self.group_error.load(Ordering::Acquire) {
            return Ok(0);
        }
        // A hardware-PMU event reads as a sequence of native-endian `u64`s in
        // Linux's strict `read_format` order: always `value`; then
        // `time_enabled` if `PERF_FORMAT_TOTAL_TIME_ENABLED`; then
        // `time_running` if `PERF_FORMAT_TOTAL_TIME_RUNNING`; then `id` if
        // `PERF_FORMAT_ID`. `PERF_FORMAT_GROUP` is unsupported in M1. With
        // `read_format == 0` this is exactly the 8-byte bare counter value
        // (M0 behaviour). The tracing variants keep the default `read_values`
        // and propagate `Unsupported` here.
        let values = self.read_values()?;

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
            return Err(StarryError::InvalidInput);
        }
        for value in &fields[..n] {
            dst.write(&value.to_ne_bytes())?;
        }
        Ok(total)
    }

    fn write(&self, _src: &mut crate::file::IoSrc) -> StarryResult<usize> {
        Err(StarryError::Unsupported)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[perf_event]".into()
    }

    fn ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> crate::StarryResult<usize> {
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
                    VmBytesMut::new(current, arg as *mut u8, core::mem::size_of::<u64>())
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
            if arg & !PERF_IOC_FLAG_GROUP != 0 {
                return Err(StarryError::InvalidInput);
            }
            if arg & PERF_IOC_FLAG_GROUP != 0 {
                self.control_group(None)?;
            } else {
                let _transaction = self.transaction.lock();
                self.reset_one()?;
            }
            return Ok(0);
        }
        let req = PerfEventIoc::try_from(cmd).map_err(|_| StarryError::InvalidInput)?;
        match req {
            PerfEventIoc::Enable => {
                if arg & PERF_IOC_FLAG_GROUP != 0 {
                    self.control_group(Some(true))?;
                } else {
                    let _transaction = self.transaction.lock();
                    self.set_enabled(true)?;
                }
            }
            PerfEventIoc::Disable => {
                if arg & PERF_IOC_FLAG_GROUP != 0 {
                    self.control_group(Some(false))?;
                } else {
                    let _transaction = self.transaction.lock();
                    self.set_enabled(false)?;
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

    fn device_mmap(&self, offset: u64, length: u64) -> StarryResult<DeviceMmap> {
        // libbpf calls mmap with offset == 0; non-zero offsets address into
        // the ringbuf, which has no meaningful sub-region exposed as a fd
        // offset (data_offset lives inside the header page).
        if offset != 0 {
            return Err(StarryError::InvalidInput);
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
        Ok(DeviceMmap::PhysicalCached(
            PhysAddrRange::from_start_size(paddr, len),
            Some(anchor),
        ))
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, on: bool) -> StarryResult {
        self.nonblocking.store(on, Ordering::Release);
        Ok(())
    }
}

/// `perf_event_open(2)` syscall entry. Copies the user `perf_event_attr` in
/// and trampolines into [`perf_event_open`], which holds the dispatcher
/// across kprobe / tracepoint / software / uprobe / hardware types.
pub fn sys_perf_event_open(
    current: &crate::task::UserTaskRef,
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> StarryResult<isize> {
    let flags = PerfOpenFlags::parse(flags)?;
    let attr = copy_perf_event_attr(current, attr_uptr)?;
    if flags.contains(PerfOpenFlags::PID_CGROUP) {
        if pid == -1 || cpu == -1 {
            return Err(StarryError::InvalidInput);
        }
        return Err(StarryError::OperationNotSupported);
    }
    perf_event_open(&attr, pid, cpu, group_fd, flags)
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
    flags: PerfOpenFlags,
) -> crate::StarryResult<isize> {
    let group_file = if group_fd == -1 {
        None
    } else {
        let file = get_file_like(group_fd).map_err(|_| StarryError::BadFileDescriptor)?;
        Some(
            file.into_any_arc()
                .downcast::<PerfEvent>()
                .map_err(|_| StarryError::BadFileDescriptor)?,
        )
    };
    let output_event = flags
        .contains(PerfOpenFlags::FD_OUTPUT)
        .then(|| group_file.clone())
        .flatten();
    let group_leader = if flags.contains(PerfOpenFlags::FD_NO_GROUP) {
        None
    } else {
        group_file
    };
    let target = PerfTarget::parse(pid, cpu).map_err(|error| match error {
        PerfTargetError::InvalidTuple => crate::StarryError::InvalidInput,
        PerfTargetError::NoSuchProcess => crate::StarryError::NoSuchProcess,
    })?;
    let target = ResolvedPerfTarget::resolve(target, ax_runtime::hal::cpu_num())?;
    let target_kind = target.kind();
    let target_cpu = target.cpu_constraint();

    // Starry does not yet deliver synchronous perf SIGTRAP notifications.
    // Reject the capability explicitly instead of accepting an event whose
    // signal side effect would be silently missing. The access policy still
    // models Linux's CAP_KILL rule so enabling the feature cannot bypass it.
    if attr.sigtrap() != 0 {
        return Err(crate::StarryError::Unsupported);
    }

    let is_hardware = attr.type_ == PerfTypeId::PERF_TYPE_HARDWARE as u32
        || attr.type_ == PerfTypeId::PERF_TYPE_HW_CACHE as u32
        || attr.type_ == PerfTypeId::PERF_TYPE_RAW as u32
        || attr.type_ == hw::ARMV8_PMUV3_PERF_TYPE
        || attr.type_ == hw::ARMV8_CORTEX_A55_PERF_TYPE
        || attr.type_ == hw::ARMV8_CORTEX_A76_PERF_TYPE;
    let validated_hw = is_hardware
        .then(|| hw::validate_perf_event_open_hw(attr, target_kind, target_cpu))
        .transpose()?;
    #[cfg(target_arch = "aarch64")]
    let direct_system_sampling = validated_hw
        .as_ref()
        .is_some_and(|validated| validated.is_sampling)
        && target_kind == target::PerfTargetKind::Cpu;
    #[cfg(not(target_arch = "aarch64"))]
    let direct_system_sampling = false;
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
            .into_starry_result()?,
        )
    };
    let new_group_backend = if is_hardware {
        PerfGroupBackend::Hardware
    } else if probe_args.as_ref().is_some_and(|args| {
        matches!(
            &args.config,
            PerfProbeConfig::PerfSwIds(sw_id) if sw::is_counting_sw(*sw_id)
        )
    }) {
        PerfGroupBackend::Software
    } else {
        PerfGroupBackend::Other
    };

    target.with_authorized(attr.sigtrap() != 0, |target| {
        let context = target.context_key()?;
        if let Some(leader) = &group_leader {
            if leader.context != Some(context)
                || leader.inherit != (attr.inherit() != 0)
                || leader.live_group_leader().is_some()
                || attr.pinned() != 0
                || attr.exclusive() != 0
            {
                return Err(StarryError::InvalidInput);
            }
            let mut leader_backend = leader.event.lock();
            let leader_group_backend = leader_backend.group_backend();
            if direct_system_sampling
                || !leader_backend.supports_group_link()
                || new_group_backend == PerfGroupBackend::Other
                || leader_group_backend == PerfGroupBackend::Other
            {
                // A tracking/probe backend has no group counter or effective-
                // enable coordinator. Its default link must not publish success.
                return Err(crate::StarryError::OperationNotSupported);
            }
            if (new_group_backend == PerfGroupBackend::Hardware
                || leader_group_backend == PerfGroupBackend::Hardware)
                && new_group_backend != leader_group_backend
            {
                // Linux can migrate mixed software/hardware groups between PMU
                // contexts. Starry has no unified coordinator yet, so reject
                // both opening orders instead of publishing an unlinked group.
                return Err(crate::StarryError::OperationNotSupported);
            }
            if target_kind == target::PerfTargetKind::Cpu
                && new_group_backend == PerfGroupBackend::Hardware
                && leader_group_backend == PerfGroupBackend::Hardware
            {
                // Flexible fixed-CPU events currently own independent workers.
                // Until they share one transactional slot scheduler, accepting
                // this link would violate whole-group scheduling and read.
                return Err(crate::StarryError::OperationNotSupported);
            }
        }
        if is_hardware && attr.pinned() != 0 {
            // Pinned scheduling needs priority over flexible events and an
            // ERROR/EOF transition on placement failure. Neither backend
            // currently implements that contract; never silently multiplex it.
            return Err(StarryError::OperationNotSupported);
        }
        // Hardware-PMU events (`PERF_TYPE_HARDWARE` / `PERF_TYPE_RAW`, plus
        // the dynamic ARM PMUv3 type `hw::ARMV8_PMUV3_PERF_TYPE`) bypass
        // `PerfProbeArgs`, which maps non-probe configs through `perf_sw_ids`.
        let enable_member = group_leader.is_some() && attr.disabled() == 0;
        let mut backend_attr = *attr;
        if group_leader.is_some() {
            // Publish the relation before an eager member can count.
            backend_attr.set_disabled(1);
        }
        let event: Box<dyn PerfEventOps> = if is_hardware {
            Box::new(hw::perf_event_open_hw(
                &backend_attr,
                target,
                validated_hw.expect("hardware perf open has validated attributes"),
            )?)
        } else {
            let args = probe_args.expect("non-hardware perf open has validated probe arguments");
            match args.type_ {
                PerfTypeId::PERF_TYPE_KPROBE => Box::new(kprobe::perf_event_open_kprobe(args)?),
                PerfTypeId::PERF_TYPE_SOFTWARE => match args.config {
                    PerfProbeConfig::PerfSwIds(sw_id) if sw::is_counting_sw(sw_id) => {
                        Box::new(sw::perf_event_open_sw(&backend_attr, sw_id, &target)?)
                    }
                    PerfProbeConfig::PerfSwIds(sw_id) if sw::is_tracking_dummy(sw_id) => {
                        Box::new(bpf::perf_event_open_tracking(args, attr, &target))
                    }
                    _ => Box::new(bpf::perf_event_open_bpf(args)),
                },
                PerfTypeId::PERF_TYPE_TRACEPOINT => {
                    Box::new(tracepoint::perf_event_open_tracepoint(args)?)
                }
                PerfTypeId::PERF_TYPE_UPROBE => {
                    Box::new(uprobe::perf_event_open_uprobe(args, target.task())?)
                }
                _ => {
                    warn!("perf_event_open: unsupported type {:?}", args.type_);
                    return Err(crate::StarryError::Unsupported);
                }
            }
        };
        // Keep membership publication and the member's initial enable inside
        // the same transaction as ioctls through any existing group FD.
        let _transaction = group_leader
            .as_ref()
            .map(|leader| leader.transaction.lock());
        let mut perf_event = PerfEvent::new(
            event,
            Some(context),
            attr.inherit() != 0,
            attr.pinned() != 0,
        )?;
        if let Some(leader) = &group_leader {
            perf_event.transaction = Arc::clone(&leader.transaction);
        }
        let perf_event = Arc::new(perf_event);
        if let Some(leader) = &group_leader {
            if leader.context != perf_event.context
                || leader.inherit != perf_event.inherit
                || leader.live_group_leader().is_some()
                || attr.pinned() != 0
                || attr.exclusive() != 0
            {
                return Err(StarryError::InvalidInput);
            }
            {
                let mut leader_backend = leader.event.lock();
                let mut member_backend = perf_event.event.lock();
                member_backend.link_group(&mut **leader_backend)?;
            }
            *perf_event.group_leader.lock() = Some(Arc::downgrade(leader));
            leader.members.lock().push(Arc::downgrade(&perf_event));
        }
        if let Some(output) = output_event {
            perf_event.set_output_target(&output)?;
        }
        if enable_member {
            perf_event.set_enabled(true)?;
        }
        let event_arc: Arc<dyn FileLike> = perf_event;
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
static PERF_FILE: LazyInit<IrqMutex<HashMap<usize, alloc::sync::Weak<dyn FileLike>>>> =
    LazyInit::new();

/// Initialize the perf-event runtime: build the fd→event lookup table.
pub fn perf_event_init() {
    PERF_FILE.init_once(IrqMutex::new(HashMap::new()));
    sw::initialize();
    #[cfg(target_arch = "aarch64")]
    {
        sideband::initialize();
        cpu_worker::init();
    }
}

/// Implementation of `bpf_perf_event_output` helper: walk the fd→event map,
/// downcast the strong upgrade to `PerfEvent`, and have the bpf-software
/// variant write a record into the ringbuf.
pub fn perf_event_output(
    _ctx: *mut c_void,
    fd: usize,
    _flags: u32,
    data: &[u8],
) -> StarryResult<()> {
    let table = PERF_FILE.get().ok_or(StarryError::NotFound)?;
    let mut map = table.lock();
    let weak = map.get(&fd).ok_or(StarryError::NotFound)?;
    let Some(file) = weak.upgrade() else {
        map.remove(&fd);
        return Err(StarryError::NotFound);
    };
    drop(map);

    let perf_event = file
        .into_any_arc()
        .downcast::<PerfEvent>()
        .map_err(|_| StarryError::InvalidInput)?;
    perf_event
        .irq_output
        .as_ref()
        .ok_or(StarryError::InvalidInput)?
        .write_event(data)
}

#[cfg(all(test, axtest))]
fn control_callback_runs_preemptible_for_test() -> bool {
    #[derive(Debug)]
    struct YieldingControl {
        preemptible: Arc<AtomicBool>,
    }

    impl Pollable for YieldingControl {
        fn poll(&self) -> axpoll::IoEvents {
            axpoll::IoEvents::empty()
        }

        unsafe fn register_shared(
            &self,
            _sink: &mut dyn axpoll::SharedRegistrationSink,
            _events: axpoll::IoEvents,
        ) {
        }
    }

    impl PerfEventOps for YieldingControl {
        fn enable(&mut self) -> crate::StarryResult<()> {
            self.preemptible.store(
                ax_runtime::task::thread::current::yield_current_cpu().is_ok(),
                Ordering::Release,
            );
            Ok(())
        }

        fn disable(&mut self) -> StarryResult<()> {
            Ok(())
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    let preemptible = Arc::new(AtomicBool::new(false));
    let event = PerfEvent::new(
        Box::new(YieldingControl {
            preemptible: Arc::clone(&preemptible),
        }),
        None,
        false,
        false,
    )
    .expect("failed to create perf control test event");
    event
        .event
        .lock()
        .enable()
        .expect("failed to enable perf control test event");
    preemptible.load(Ordering::Acquire)
}

/// Executable kernel mapping used by rbpf JIT programs on x86_64.
#[allow(unused)]
struct BPFJitMemory {
    num_pages: usize,
    pages: VirtAddr,
}

#[allow(unused)]
impl BPFJitMemory {
    fn new(num_pages: usize) -> StarryResult<Self> {
        let hint = ax_runtime::hal::mem::virtual_address_space()
            .expect("kernel virtual address layout is initialized")
            .kernel()
            .start;
        let virt_start = ax_runtime::kernel_mapping::allocate_kernel_range(
            hint,
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
        ax_runtime::kernel_mapping::unmap_kernel_range(self.pages, self.num_pages * PAGE_SIZE_4K)
            .expect("failed to unmap BPF JIT memory");
    }
}

#[cfg(all(test, axtest))]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn control_callback_runs_preemptible() {
        assert!(super::control_callback_runs_preemptible_for_test());
    }
}
