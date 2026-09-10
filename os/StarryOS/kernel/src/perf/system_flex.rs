//! Task-context multiplexing for flexible fixed-CPU PMU events.

use alloc::{format, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_runtime::task::sched::{CpuId, CpuSet};

use super::{hw_owner::Counter, target::PerfCpuId};
use crate::sync::{IrqMutex, NoPreemptIrqSave, PreemptGuard};

const SLICE: Duration = Duration::from_millis(2);

struct ActiveSlice {
    counter: Counter,
    started_at: u64,
    registration: super::sampling_lifecycle::SampleRegistration,
}

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
    extender: Arc<IrqMutex<super::counting::CounterExtender>>,
    active: IrqMutex<Option<ActiveSlice>>,
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
            extender: Arc::new(IrqMutex::new(super::counting::CounterExtender::new())),
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
                    self.extender.lock().reset();
                    ax_cpu::pmu::overflow::clear(1 << slot);
                    if super::sampling::enable_local_pmu_irq().is_err() {
                        super::percpu::free_current_programmable(slot);
                        false
                    } else {
                        let registration =
                            super::sampling::register_counting(slot, Arc::clone(&self.extender))
                                .expect(
                                    "reserved system PMU slot must have an empty overflow registry",
                                );
                        ax_cpu::pmu::overflow::enable_irq(slot);
                        counter.enable();
                        *active = Some(ActiveSlice {
                            counter,
                            started_at: now_ns(),
                            registration,
                        });
                        true
                    }
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
        drop(self.finish_slice_observed(|| {}));
    }

    fn finish_slice_observed(
        &self,
        before_commit: impl FnOnce(),
    ) -> Option<Arc<IrqMutex<super::counting::CounterExtender>>> {
        let _guard = NoPreemptIrqSave::new();
        // Publish None only after hardware quiescence, accounting and slot free.
        let mut active_state = self.active.lock();
        let active = active_state.take()?;
        before_commit();
        let slot = active
            .counter
            .programmable_index()
            .expect("flexible programmable slot");
        ax_cpu::pmu::overflow::disable_irq(slot);
        active.counter.disable();
        let value = self.read_active_counter(active.counter);
        let retired = super::sampling::detach_counting(active.registration)
            .expect("system PMU overflow registration must match its active slice");
        self.accumulated.fetch_add(value, Ordering::AcqRel);
        self.time_running
            .fetch_add(now_ns().saturating_sub(active.started_at), Ordering::AcqRel);
        super::percpu::free_current_programmable(slot);
        Some(retired)
    }

    pub(super) const fn owner(&self) -> PerfCpuId {
        self.owner
    }

    pub(super) fn enable(&self) {
        if !self.enabled.swap(true, Ordering::AcqRel) {
            self.enabled_since.store(now_ns(), Ordering::Release);
        }
    }

    pub(super) fn disable(&self) -> crate::StarryResult<()> {
        self.control_on_owner(ControlOperation::Disable)
    }

    pub(super) fn reset(&self) -> crate::StarryResult<()> {
        self.control_on_owner(ControlOperation::Reset)
    }

    fn reset_on_owner(&self) {
        let _guard = NoPreemptIrqSave::new();
        let active = self.active.lock();
        if let Some(active) = active.as_ref() {
            active.counter.disable();
            active.counter.reset();
            ax_cpu::pmu::overflow::clear(1 << active.registration.counter());
        }
        self.accumulated.store(0, Ordering::Release);
        self.extender.lock().reset();
        // Linux RESET preserves the enabled state and cumulative time. Keep
        // the current lease running even if a FIFO caller excludes the worker.
        if let Some(active) = active.as_ref() {
            active.counter.enable();
        }
    }

    fn control_on_owner(&self, operation: ControlOperation) -> crate::StarryResult<()> {
        let mut request = ControlRequest {
            counter: self,
            operation,
            retired: None,
        };
        let result = {
            // Pin the local fast-path CPU without masking IPIs during a remote
            // wait. No lock needed by the callback is held across the call.
            let _pin = PreemptGuard::new();
            // SAFETY: the synchronous call borrows this stack request until
            // completion, including error cancellation. Only the callback
            // accesses it meanwhile. It performs bounded IRQ-safe PMU work;
            // detached ownership is returned here for task-context destruction.
            unsafe {
                ax_hal::irq::run_on_cpu_sync(
                    ax_hal::irq::CpuId(self.owner.as_usize()),
                    control_callback,
                    (&raw mut request).cast(),
                )
            }
        };
        drop(request.retired);
        result.map_err(|error| match error {
            ax_hal::irq::IrqError::CpuOffline => crate::StarryError::NoSuchDeviceOrAddress,
            ax_hal::irq::IrqError::InvalidCpu => crate::StarryError::InvalidInput,
            ax_hal::irq::IrqError::Unsupported => crate::StarryError::Unsupported,
            ax_hal::irq::IrqError::Timeout => crate::StarryError::TimedOut,
            _ => crate::StarryError::Io,
        })
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
        if let Some(active) = active.as_ref() {
            value = value.saturating_add(self.read_active_counter(active.counter));
            running = running.saturating_add(observed_at.saturating_sub(active.started_at));
        }
        (value, enabled, running)
    }

    pub(super) fn close(&self) -> crate::StarryResult<()> {
        self.disable()?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }

    fn read_active_counter(&self, counter: Counter) -> u64 {
        let mut extender = self.extender.lock();
        let slot = counter
            .programmable_index()
            .expect("flexible programmable slot");
        let bit = 1 << slot;
        if ax_cpu::pmu::overflow::status() & bit != 0 {
            ax_cpu::pmu::overflow::clear(bit);
            extender.record_overflow();
        }
        let (_, width) = counter.mmap_metadata();
        extender.value(counter.read(), width)
    }
}

enum ControlOperation {
    Disable,
    Reset,
}

struct ControlRequest<'a> {
    counter: &'a SystemFlexCounter,
    operation: ControlOperation,
    retired: Option<Arc<IrqMutex<super::counting::CounterExtender>>>,
}

/// # Safety
/// `arg` must point to the exclusive, live request borrowed by
/// `control_on_owner`; this callback must execute on its counter's owner CPU.
unsafe fn control_callback(arg: *mut ()) {
    // SAFETY: control_on_owner lends an initialized, aligned stack request and
    // does not access or destroy it until the synchronous callback completes.
    let request = unsafe { &mut *arg.cast::<ControlRequest<'_>>() };
    let _guard = NoPreemptIrqSave::new();
    let counter = request.counter;
    match request.operation {
        ControlOperation::Disable => {
            counter.enabled.store(false, Ordering::Release);
            request.retired = counter.finish_slice_observed(|| {});
            let since = counter.enabled_since.swap(0, Ordering::AcqRel);
            if since != 0 {
                counter.time_enabled.fetch_add(
                    now_ns().saturating_sub(since),
                    Ordering::AcqRel,
                );
            }
        }
        ControlOperation::Reset => counter.reset_on_owner(),
    }
}

fn now_ns() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

#[cfg(all(test, axtest))]
mod tests {
    use super::*;

    #[axtest::axtest]
    fn stop_is_not_published_before_hardware_commit() {
        let counter = SystemFlexCounter {
            owner: PerfCpuId::new(0),
            event: 0x11,
            exclude_user: false,
            exclude_kernel: false,
            enabled: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            enabled_since: AtomicU64::new(0),
            accumulated: AtomicU64::new(0),
            time_enabled: AtomicU64::new(0),
            time_running: AtomicU64::new(0),
            extender: Arc::new(IrqMutex::new(super::super::counting::CounterExtender::new())),
            active: IrqMutex::new(None),
        };
        let _guard = NoPreemptIrqSave::new();
        super::super::percpu::ensure_current_cpu_initialized().unwrap();
        let slot = super::super::percpu::alloc_current_programmable().unwrap();
        let hardware = Counter::Programmable(slot);
        hardware.configure(Some(0x11), false, false).unwrap();
        let registration =
            super::super::sampling::register_counting(slot, Arc::clone(&counter.extender)).unwrap();
        *counter.active.lock() = Some(ActiveSlice {
            counter: hardware,
            started_at: now_ns(),
            registration,
        });
        hardware.enable();
        let published_early = core::cell::Cell::new(false);
        drop(counter.finish_slice_observed(|| {
            published_early.set(
                counter
                    .active
                    .try_lock()
                    .is_some_and(|active| active.is_none()),
            );
        }));
        assert!(
            !published_early.get(),
            "disable must not observe stopped before the slice commits"
        );
        assert!(counter.active.lock().is_none());
    }
}
