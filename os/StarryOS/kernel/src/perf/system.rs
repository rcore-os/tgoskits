//! Per-CPU Linux perf context for system-wide ARM PMUv3 counting events.
//!
//! Flexible events own logical state, not a permanent PMU slot. The owner
//! CPU's timer callback rotates eligible groups through the programmable
//! counters and accounts `time_enabled` separately from `time_running`.
//! Pinned events have priority and enter the Linux scheduling-error state when
//! their complete group cannot be placed.

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence},
};

use ax_alloc::GlobalPage;
use ax_hal::mem::virt_to_phys;
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use kbpf_basic::linux_bpf::perf_event_mmap_page;

use super::{PerfReadValues, hw::PmuEventSpec, sampling::MAX_SAMPLE_READ_EVENTS};
use crate::{StarryError, StarryResult, sync::IrqMutex};

const NO_SLOT: usize = usize::MAX;

static SYSTEM_EVENTS: LazyInit<IrqMutex<Vec<Weak<SystemCounter>>>> = LazyInit::new();
static SYSTEM_EVENT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Immutable construction parameters for one CPU-wide counting event.
pub struct SystemCounterConfig {
    pub owner_cpu: usize,
    pub event: PmuEventSpec,
    pub exclude_user: bool,
    pub exclude_kernel: bool,
    pub read_format: u64,
    pub pinned: bool,
    pub enabled: bool,
}

/// Logical system-wide event scheduled onto an owner CPU's PMU.
#[derive(Debug)]
pub struct SystemCounter {
    owner_cpu: usize,
    event: PmuEventSpec,
    exclude_user: bool,
    exclude_kernel: bool,
    read_format: u64,
    pinned: bool,
    enabled: AtomicBool,
    running: AtomicBool,
    dead: AtomicBool,
    scheduling_error: AtomicBool,
    slot: AtomicUsize,
    accumulated: AtomicU64,
    enabled_at_ns: AtomicU64,
    run_since_ns: AtomicU64,
    time_enabled_ns: AtomicU64,
    time_running_ns: AtomicU64,
    group_leader: IrqMutex<Option<Weak<SystemCounter>>>,
    group_members: IrqMutex<Vec<Weak<SystemCounter>>>,
    rdpmc_page: IrqMutex<Option<Weak<GlobalPage>>>,
}

pub fn initialize() {
    SYSTEM_EVENTS.init_once(IrqMutex::new(Vec::new()));
}

#[inline]
fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

impl SystemCounter {
    /// Creates and registers an event without reserving a permanent PMU slot.
    pub fn open(cfg: SystemCounterConfig) -> StarryResult<Arc<Self>> {
        let info = super::percpu::cpu_info(cfg.owner_cpu)
            .ok_or(StarryError::OperationNotSupported)?;
        let _ = cfg.event.resolve(info)?;
        let event = Arc::new(Self {
            owner_cpu: cfg.owner_cpu,
            event: cfg.event,
            exclude_user: cfg.exclude_user,
            exclude_kernel: cfg.exclude_kernel,
            read_format: cfg.read_format,
            pinned: cfg.pinned,
            enabled: AtomicBool::new(false),
            running: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            scheduling_error: AtomicBool::new(false),
            slot: AtomicUsize::new(NO_SLOT),
            accumulated: AtomicU64::new(0),
            enabled_at_ns: AtomicU64::new(0),
            run_since_ns: AtomicU64::new(0),
            time_enabled_ns: AtomicU64::new(0),
            time_running_ns: AtomicU64::new(0),
            group_leader: IrqMutex::new(None),
            group_members: IrqMutex::new(Vec::new()),
            rdpmc_page: IrqMutex::new(None),
        });
        {
            let mut events = SYSTEM_EVENTS
                .get()
                .expect("perf system context not initialized")
                .lock();
            events.retain(|entry| entry.strong_count() != 0);
            events.push(Arc::downgrade(&event));
        }
        SYSTEM_EVENT_COUNT.fetch_add(1, Ordering::AcqRel);
        if cfg.enabled
            && let Err(error) = event.set_enabled()
        {
            event.release();
            return Err(error);
        }
        Ok(event)
    }

    pub fn link_group(leader: &Arc<Self>, member: &Arc<Self>) -> StarryResult<()> {
        if leader.owner_cpu != member.owner_cpu
            || leader.dead.load(Ordering::Acquire)
            || member.dead.load(Ordering::Acquire)
        {
            return Err(StarryError::InvalidInput);
        }
        let mut members = leader.group_members.lock();
        members.retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|event| !event.dead.load(Ordering::Acquire))
        });
        if members.len() + 1 >= MAX_SAMPLE_READ_EVENTS {
            return Err(StarryError::InvalidInput);
        }
        *member.group_leader.lock() = Some(Arc::downgrade(leader));
        members.push(Arc::downgrade(member));
        drop(members);

        // The member may have started as a standalone open-enabled event
        // before the file layer supplied its group link. Rebuild the owner
        // context immediately so an OFF leader gates it just as Linux does.
        leader.reschedule_on_owner()
    }

    pub fn set_enabled(&self) -> StarryResult<()> {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_at_ns.store(now_ns(), Ordering::Release);
        }
        self.scheduling_error.store(false, Ordering::Release);
        self.reschedule_on_owner()
    }

    pub fn set_disabled(&self) -> StarryResult<()> {
        if self.enabled.swap(false, Ordering::AcqRel) {
            let since = self.enabled_at_ns.swap(0, Ordering::AcqRel);
            if since != 0 {
                self.time_enabled_ns
                    .fetch_add(now_ns().saturating_sub(since), Ordering::AcqRel);
            }
        }
        self.reschedule_on_owner()
    }

    pub fn reset(&self) -> StarryResult<()> {
        let owner = self.owner_cpu;
        // SAFETY: the closure is bounded, allocation-free, and accesses only
        // the target CPU's PMU plus this live event.
        unsafe {
            super::percpu::run_on_cpu_sync(owner, || {
                reset_one(self);
            })
        }
    }

    /// Reset the leader and every live sibling while the owner CPU has local
    /// IRQs disabled, matching Linux's context-locked group RESET operation.
    pub fn reset_group(&self) -> StarryResult<()> {
        let owner = self.owner_cpu;
        // SAFETY: the closure is bounded by the fixed maximum group size and
        // touches only owner-local PMU state plus IRQ-safe event locks.
        unsafe {
            super::percpu::run_on_cpu_sync(owner, || {
                reset_one(self);
                let members = self.group_members.lock();
                for member in members.iter().filter_map(Weak::upgrade) {
                    if !member.dead.load(Ordering::Acquire) {
                        reset_one(&member);
                    }
                }
            })
        }
    }

    pub fn read_values(&self) -> StarryResult<PerfReadValues> {
        let owner = self.owner_cpu;
        // SAFETY: snapshotting performs bounded atomics and one owner-local PMU
        // read. `self` remains live until the synchronous call returns.
        unsafe { super::percpu::run_on_cpu_sync(owner, || self.snapshot_on_owner()) }
    }

    pub fn device_mmap(
        &self,
        len: usize,
    ) -> StarryResult<(PhysAddr, Arc<dyn Any + Send + Sync>)> {
        if len != PAGE_SIZE_4K {
            return Err(StarryError::InvalidInput);
        }
        if self.rdpmc_page.lock().as_ref().and_then(Weak::upgrade).is_some() {
            return Err(StarryError::ResourceBusy);
        }
        let mut page = GlobalPage::alloc_contiguous(1, PAGE_SIZE_4K)
            .map_err(|_| StarryError::NoMemory)?;
        page.zero();
        let paddr = virt_to_phys(page.start_vaddr());
        let header = page.start_vaddr().as_usize() as *mut perf_event_mmap_page;
        // SAFETY: this is a fresh, zeroed, page-sized allocation that is not
        // published to userspace until after all fixed header fields are set.
        unsafe {
            core::ptr::addr_of_mut!((*header).version).write(1);
            core::ptr::addr_of_mut!((*header).compat_version).write(0);
            core::ptr::addr_of_mut!((*header).pmc_width).write(32);
            core::ptr::addr_of_mut!((*header).size).write(
                core::mem::offset_of!(perf_event_mmap_page, __reserved) as u32,
            );
            core::ptr::addr_of_mut!((*header).data_offset).write(PAGE_SIZE_4K as u64);
            core::ptr::addr_of_mut!((*header).data_size).write(0);
        }
        let page = Arc::new(page);
        {
            let mut mapped = self.rdpmc_page.lock();
            if mapped.as_ref().and_then(Weak::upgrade).is_some() {
                return Err(StarryError::ResourceBusy);
            }
            self.write_rdpmc_snapshot(&page, self.running.load(Ordering::Acquire));
            *mapped = Some(Arc::downgrade(&page));
        }
        let anchor: Arc<dyn Any + Send + Sync> = page;
        Ok((paddr, anchor))
    }

    pub fn release(&self) {
        if self.dead.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.set_disabled();
        *self.rdpmc_page.lock() = None;
        SYSTEM_EVENT_COUNT.fetch_sub(1, Ordering::AcqRel);
    }

    fn reschedule_on_owner(&self) -> StarryResult<()> {
        let owner = self.owner_cpu;
        // SAFETY: group rescheduling is bounded by the fixed event registry,
        // uses IRQ-safe locks, performs no allocation, and only touches the
        // target CPU's PMU.
        let placed = unsafe {
            super::percpu::run_on_cpu_sync(owner, || {
                let root = live_group_leader(self);
                let root = root.as_deref().unwrap_or(self);
                let now = now_ns();

                // Linux's CPU context is placed before the current task's
                // flexible context. Rebuild both flexible layers so a newly
                // enabled CPU-pinned event can evict lower-priority work.
                disarm_flexible_current(now);
                super::task::disarm_current_flexible(now);
                disarm_group(root, now);

                let placed = if !root.enabled.load(Ordering::Acquire) {
                    true
                } else if effective_pinned(root) {
                    schedule_group(root, now)
                } else {
                    schedule_flexible_current(now);
                    true
                };
                if !placed {
                    enter_scheduling_error(root, now);
                }
                schedule_flexible_current(now);
                super::task::schedule_current_flexible(now);
                placed
            })
        }?;
        if !placed {
            return Err(StarryError::ResourceBusy);
        }
        Ok(())
    }

    fn snapshot_on_owner(&self) -> PerfReadValues {
        let now = now_ns();
        let mut value = self.accumulated.load(Ordering::Acquire);
        let slot = self.slot.load(Ordering::Acquire);
        if self.running.load(Ordering::Acquire) && slot != NO_SLOT {
            value = value.saturating_add(ax_cpu::pmu::counter::read(slot));
        }
        let mut time_enabled = self.time_enabled_ns.load(Ordering::Acquire);
        let enabled_at = self.enabled_at_ns.load(Ordering::Acquire);
        if self.enabled.load(Ordering::Acquire) && enabled_at != 0 {
            time_enabled = time_enabled.saturating_add(now.saturating_sub(enabled_at));
        }
        let mut time_running = self.time_running_ns.load(Ordering::Acquire);
        let run_since = self.run_since_ns.load(Ordering::Acquire);
        if self.running.load(Ordering::Acquire) && run_since != 0 {
            time_running = time_running.saturating_add(now.saturating_sub(run_since));
        }
        PerfReadValues {
            eof: self.scheduling_error.load(Ordering::Acquire),
            value,
            time_enabled,
            time_running,
            lost: 0,
            read_format: self.read_format,
        }
    }

    fn publish_rdpmc_page(&self, active: bool) {
        let page = self.rdpmc_page.lock();
        let Some(page) = page.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        self.write_rdpmc_snapshot(&page, active);
    }

    fn write_rdpmc_snapshot(&self, page: &GlobalPage, active: bool) {
        let header = page.start_vaddr().as_usize() as *mut perf_event_mmap_page;
        let slot = self.slot.load(Ordering::Acquire);
        let index = if active && slot != NO_SLOT {
            slot as u32 + 1
        } else {
            0
        };
        let capabilities = (1u64 << 1) | if index != 0 { 1u64 << 2 } else { 0 };
        let offset = self.accumulated.load(Ordering::Acquire) as i64;
        let now = now_ns();
        let mut time_enabled = self.time_enabled_ns.load(Ordering::Acquire);
        let enabled_at = self.enabled_at_ns.load(Ordering::Acquire);
        if self.enabled.load(Ordering::Acquire) && enabled_at != 0 {
            time_enabled = time_enabled.saturating_add(now.saturating_sub(enabled_at));
        }
        let mut time_running = self.time_running_ns.load(Ordering::Acquire);
        let run_since = self.run_since_ns.load(Ordering::Acquire);
        if active && run_since != 0 {
            time_running = time_running.saturating_add(now.saturating_sub(run_since));
        }
        // SAFETY: `page` pins the page-sized allocation and `rdpmc_page`
        // serializes writers. The odd/even atomic sequence implements Linux's
        // userspace seqlock protocol for the following volatile fields.
        unsafe {
            let sequence = AtomicU32::from_ptr(core::ptr::addr_of_mut!((*header).lock));
            let odd = sequence.load(Ordering::Relaxed).wrapping_add(1) | 1;
            sequence.store(odd, Ordering::SeqCst);
            core::ptr::addr_of_mut!((*header).index).write_volatile(index);
            core::ptr::addr_of_mut!((*header).offset).write_volatile(offset);
            core::ptr::addr_of_mut!((*header).time_enabled).write_volatile(time_enabled);
            core::ptr::addr_of_mut!((*header).time_running).write_volatile(time_running);
            core::ptr::addr_of_mut!((*header).__bindgen_anon_1.capabilities)
                .write_volatile(capabilities);
            fence(Ordering::Release);
            sequence.store(odd.wrapping_add(1), Ordering::Release);
        }
    }
}

fn reset_one(event: &SystemCounter) {
    // Keep the software accumulator and live hardware reset in the same
    // owner-CPU IRQ-disabled transaction. Otherwise timer rotation could fold
    // a pre-reset value back into the accumulator after it was cleared.
    event.accumulated.store(0, Ordering::Release);
    let slot = event.slot.load(Ordering::Acquire);
    if event.running.load(Ordering::Acquire) && slot != NO_SLOT {
        ax_cpu::pmu::counter::reset(slot);
    }
    event.publish_rdpmc_page(event.running.load(Ordering::Acquire));
}

fn enter_scheduling_error(event: &SystemCounter, now: u64) {
    if event.enabled.swap(false, Ordering::AcqRel) {
        let enabled_at = event.enabled_at_ns.swap(0, Ordering::AcqRel);
        if enabled_at != 0 {
            event
                .time_enabled_ns
                .fetch_add(now.saturating_sub(enabled_at), Ordering::AcqRel);
        }
    }
    event.scheduling_error.store(true, Ordering::Release);
    event.publish_rdpmc_page(false);
}

fn live_group_leader(event: &SystemCounter) -> Option<Arc<SystemCounter>> {
    event
        .group_leader
        .lock()
        .as_ref()
        .and_then(Weak::upgrade)
        .filter(|leader| !leader.dead.load(Ordering::Acquire))
}

fn is_live_group_member(event: &SystemCounter) -> bool {
    live_group_leader(event).is_some()
}

fn effective_pinned(event: &SystemCounter) -> bool {
    event.pinned || live_group_leader(event).is_some_and(|leader| leader.pinned)
}

fn eligible(event: &SystemCounter) -> bool {
    event.owner_cpu == ax_hal::percpu::this_cpu_id()
        && event.enabled.load(Ordering::Acquire)
        && !event.dead.load(Ordering::Acquire)
        && !event.scheduling_error.load(Ordering::Acquire)
}

fn arm_slice(event: &SystemCounter, slot: usize, now: u64) {
    let Some(info) = super::percpu::current_info() else {
        super::percpu::free_programmable(slot);
        return;
    };
    let Ok(encoding) = event.event.resolve(info) else {
        super::percpu::free_programmable(slot);
        return;
    };
    ax_cpu::pmu::counter::configure(
        slot,
        encoding,
        event.exclude_user,
        event.exclude_kernel,
    );
    event.slot.store(slot, Ordering::Release);
    event.run_since_ns.store(now, Ordering::Release);
    event.running.store(true, Ordering::Release);
    ax_cpu::pmu::counter::enable(slot);
    event.publish_rdpmc_page(true);
}

fn disarm_slice(event: &SystemCounter, now: u64) {
    let slot = event.slot.load(Ordering::Acquire);
    if slot == NO_SLOT {
        event.running.store(false, Ordering::Release);
        return;
    }
    ax_cpu::pmu::counter::disable(slot);
    event
        .accumulated
        .fetch_add(ax_cpu::pmu::counter::read(slot), Ordering::AcqRel);
    let since = event.run_since_ns.swap(0, Ordering::AcqRel);
    if since != 0 {
        event
            .time_running_ns
            .fetch_add(now.saturating_sub(since), Ordering::AcqRel);
    }
    super::percpu::free_programmable(slot);
    event.slot.store(NO_SLOT, Ordering::Release);
    event.running.store(false, Ordering::Release);
    event.publish_rdpmc_page(false);
}

fn group_members(
    leader: &SystemCounter,
) -> ([Option<Arc<SystemCounter>>; MAX_SAMPLE_READ_EVENTS], usize) {
    let mut snapshot = core::array::from_fn(|_| None);
    let mut len = 0;
    let members = leader.group_members.lock();
    for member in members.iter().filter_map(Weak::upgrade) {
        if member.dead.load(Ordering::Acquire) || !member.enabled.load(Ordering::Acquire) {
            continue;
        }
        if len + 1 == MAX_SAMPLE_READ_EVENTS {
            break;
        }
        snapshot[len] = Some(member);
        len += 1;
    }
    (snapshot, len)
}

fn disarm_group(leader: &SystemCounter, now: u64) {
    if leader.running.load(Ordering::Acquire) {
        disarm_slice(leader, now);
    }
    let members = leader.group_members.lock();
    for member in members.iter().filter_map(Weak::upgrade) {
        if member.running.load(Ordering::Acquire) {
            disarm_slice(&member, now);
        }
    }
}

fn schedule_group(leader: &SystemCounter, now: u64) -> bool {
    if !eligible(leader) {
        return true;
    }
    let (members, member_len) = group_members(leader);
    if members[..member_len]
        .iter()
        .filter_map(Option::as_deref)
        .any(|member| !eligible(member))
    {
        return false;
    }
    disarm_group(leader, now);
    let mut slots = [NO_SLOT; MAX_SAMPLE_READ_EVENTS];
    for index in 0..=member_len {
        let Some(slot) = super::percpu::alloc_programmable() else {
            for reserved in slots[..index].iter().copied() {
                super::percpu::free_programmable(reserved);
            }
            return false;
        };
        slots[index] = slot;
    }
    for index in (0..member_len).rev() {
        arm_slice(members[index].as_deref().unwrap(), slots[index + 1], now);
    }
    arm_slice(leader, slots[0], now);
    true
}

/// Stops every running flexible CPU event before task-context placement.
pub fn disarm_flexible_current(now: u64) {
    if SYSTEM_EVENT_COUNT.load(Ordering::Acquire) == 0 {
        return;
    }
    let events = SYSTEM_EVENTS
        .get()
        .expect("perf system context not initialized")
        .lock();
    for event in events.iter().filter_map(Weak::upgrade) {
        if event.owner_cpu == ax_hal::percpu::this_cpu_id()
            && event.running.load(Ordering::Acquire)
            && !effective_pinned(&event)
        {
            disarm_slice(&event, now);
        }
    }
}

/// Fills remaining PMU slots with flexible CPU event groups in round-robin order.
pub fn schedule_flexible_current(now: u64) {
    if SYSTEM_EVENT_COUNT.load(Ordering::Acquire) == 0 {
        return;
    }
    let events = SYSTEM_EVENTS
        .get()
        .expect("perf system context not initialized")
        .lock();
    if events.is_empty() {
        return;
    }
    let start = super::percpu::next_rotation_start(events.len());
    for offset in 0..events.len() {
        let Some(event) = events[(start + offset) % events.len()].upgrade() else {
            continue;
        };
        if effective_pinned(&event)
            || is_live_group_member(&event)
            || event.running.load(Ordering::Acquire)
            || !eligible(&event)
        {
            continue;
        }
        let _ = schedule_group(&event, now);
    }
}
