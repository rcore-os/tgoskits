//! Task-context multiplexing for flexible fixed-CPU PMU events.

use alloc::{format, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_runtime::task::sched::{CpuId, CpuSet};

use super::{hw_owner::Counter, target::PerfCpuId};
use crate::sync::{IrqMutex, NoPreemptIrqSave};

const SLICE: Duration = Duration::from_millis(2);

/// One logical fixed-CPU event whose physical slot changes between slices.
pub(super) struct SystemFlexCounter {
    owner: PerfCpuId,
    event: u16,
    exclude_user: bool,
    exclude_kernel: bool,
    enabled: AtomicBool,
    closed: AtomicBool,
    enabled_since: AtomicU64,
    accumulated: AtomicU64,
    time_enabled: AtomicU64,
    time_running: AtomicU64,
    active: IrqMutex<Option<(Counter, u64)>>,
}

impl core::fmt::Debug for SystemFlexCounter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SystemFlexCounter")
            .field("owner", &self.owner)
            .field("event", &self.event)
            .field("enabled", &self.enabled.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl SystemFlexCounter {
    pub(super) fn new(
        owner: PerfCpuId,
        event: u16,
        exclude_user: bool,
        exclude_kernel: bool,
    ) -> Arc<Self> {
        let counter = Arc::new(Self {
            owner,
            event,
            exclude_user,
            exclude_kernel,
            enabled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            enabled_since: AtomicU64::new(0),
            accumulated: AtomicU64::new(0),
            time_enabled: AtomicU64::new(0),
            time_running: AtomicU64::new(0),
            active: IrqMutex::new(None),
        });
        let worker_counter = Arc::clone(&counter);
        let mut affinity = CpuSet::empty(ax_runtime::hal::cpu_num());
        assert!(affinity.insert(CpuId::new(owner.as_usize() as u32)));
        crate::task::spawn_kernel_thread_with_affinity(
            move || worker_counter.run(),
            format!("perf-flex/{}", owner.as_usize()),
            affinity,
        );
        counter
    }

    fn run(self: Arc<Self>) {
        while !self.closed.load(Ordering::Acquire) {
            let armed = {
                let _guard = NoPreemptIrqSave::new();
                let mut active = self.active.lock();
                if !self.enabled.load(Ordering::Acquire) || active.is_some() {
                    false
                } else if let Some(slot) = super::percpu::alloc_current_programmable() {
                    let counter = Counter::Programmable(slot);
                    counter
                        .configure(Some(self.event), self.exclude_user, self.exclude_kernel)
                        .expect("validated flexible system PMU event");
                    counter.enable();
                    *active = Some((counter, now_ns()));
                    true
                } else {
                    false
                }
            };

            if armed {
                crate::task::sleep(SLICE);
                self.finish_slice();
                crate::task::yield_now();
            } else {
                crate::task::sleep(SLICE);
            }
        }
        self.finish_slice();
    }

    fn finish_slice(&self) {
        let _guard = NoPreemptIrqSave::new();
        let Some((counter, started_at)) = self.active.lock().take() else {
            return;
        };
        counter.disable();
        self.accumulated.fetch_add(counter.read(), Ordering::AcqRel);
        self.time_running
            .fetch_add(now_ns().saturating_sub(started_at), Ordering::AcqRel);
        super::percpu::free_current_programmable(
            counter.programmable_index().expect("flexible programmable slot"),
        );
    }

    pub(super) const fn owner(&self) -> PerfCpuId {
        self.owner
    }

    pub(super) fn enable(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_since.store(now_ns(), Ordering::Release);
        }
    }

    pub(super) fn disable(&self) {
        if !self.enabled.swap(false, Ordering::AcqRel) {
            return;
        }
        while self.active.lock().is_some() {
            crate::task::yield_now();
        }
        let since = self.enabled_since.swap(0, Ordering::AcqRel);
        if since != 0 {
            self.time_enabled
                .fetch_add(now_ns().saturating_sub(since), Ordering::AcqRel);
        }
    }

    pub(super) fn reset(&self) {
        let enabled = self.enabled.load(Ordering::Acquire);
        self.disable();
        self.accumulated.store(0, Ordering::Release);
        self.time_enabled.store(0, Ordering::Release);
        self.time_running.store(0, Ordering::Release);
        if enabled {
            self.enable();
        }
    }

    pub(super) fn read(self: &Arc<Self>) -> crate::StarryResult<(u64, u64, u64)> {
        super::cpu_worker::read_system_flexible(Arc::clone(self))
    }

    /// Reads the committed totals plus the currently active hardware slice.
    ///
    /// The caller must execute on `self.owner` with local PMU exclusion. The
    /// active lock serializes this snapshot with the slice worker's
    /// disable/read/free transition, so the raw value and running time are
    /// observed from the same slice generation without ending that slice.
    pub(super) fn read_on_owner(&self) -> (u64, u64, u64) {
        debug_assert_eq!(
            self.owner.as_usize(),
            ax_runtime::hal::percpu::this_cpu_id()
        );
        let active = self.active.lock();
        let observed_at = now_ns();
        let mut enabled = self.time_enabled.load(Ordering::Acquire);
        let since = self.enabled_since.load(Ordering::Acquire);
        if since != 0 {
            enabled = enabled.saturating_add(observed_at.saturating_sub(since));
        }
        let mut value = self.accumulated.load(Ordering::Acquire);
        let mut running = self.time_running.load(Ordering::Acquire);
        if let Some((counter, started_at)) = *active {
            value = value.saturating_add(counter.read());
            running = running.saturating_add(observed_at.saturating_sub(started_at));
        }
        (value, enabled, running)
    }

    pub(super) fn close(&self) {
        self.disable();
        self.closed.store(true, Ordering::Release);
    }
}

fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}
