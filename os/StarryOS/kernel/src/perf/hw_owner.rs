//! CPU-local ARM PMUv3 operations and value-only rendezvous requests.

use super::{
    sampling::{self, SampleOutput, SampleSlot},
    sampling_lifecycle::SampleRegistration,
};

/// Hardware counter selected for one PMU event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Counter {
    Cycle,
    Programmable(usize),
}

impl Counter {
    pub(super) fn configure(
        self,
        event: Option<u16>,
        exclude_user: bool,
        exclude_kernel: bool,
    ) -> crate::StarryResult<()> {
        let event = match (self, event) {
            (Self::Cycle, None) => 0x11,
            (Self::Programmable(_), Some(event)) => event,
            _ => return Err(crate::StarryError::BadState),
        };
        on_pmu(|pmu| {
            let id = self.id(pmu).map_err(pmu_error)?;
            pmu.configure(
                id,
                ax_cpu::pmu::EventConfig {
                    event,
                    exclude_user,
                    exclude_kernel,
                    include_hypervisor: false,
                },
            )
            .map_err(pmu_error)?;
            pmu.write(id, 0).map_err(pmu_error)?;
            pmu.start();
            Ok(())
        })
    }

    fn id(self, pmu: &ax_cpu::pmu::Pmu) -> Result<ax_cpu::pmu::CounterId, ax_cpu::pmu::PmuError> {
        match self {
            Self::Cycle => Ok(ax_cpu::pmu::CounterId::CYCLE),
            Self::Programmable(index) => pmu.counter(index),
        }
    }

    pub(super) fn enable(self) {
        on_pmu(|pmu| pmu.enable(self.id(pmu).expect("reserved counter")))
            .expect("enable reserved PMU counter");
    }

    pub(super) fn disable(self) {
        on_pmu(|pmu| pmu.disable(self.id(pmu).expect("reserved counter")))
            .expect("disable reserved PMU counter");
    }

    pub(super) fn reset(self) {
        on_pmu(|pmu| pmu.write(self.id(pmu).expect("reserved counter"), 0))
            .expect("reset reserved PMU counter");
    }

    pub(super) fn read(self) -> u64 {
        on_pmu(|pmu| pmu.read(self.id(pmu).expect("reserved counter")))
            .expect("read reserved PMU counter")
    }

    pub(super) const fn programmable_index(self) -> Option<usize> {
        match self {
            Self::Cycle => None,
            Self::Programmable(n) => Some(n),
        }
    }

    pub(super) const fn mmap_metadata(self) -> (u32, u16) {
        match self {
            // Linux publishes `event->hw.idx + 1`; the architectural cycle
            // counter is index 31.
            Self::Cycle => (32, 64),
            Self::Programmable(n) => (n as u32 + 1, 32),
        }
    }
}

/// Value-only request to configure a system-wide PMU event on its owner CPU.
pub(super) struct SystemPmuConfigure {
    pub(super) counter: Counter,
    pub(super) event: Option<u16>,
    pub(super) exclude_user: bool,
    pub(super) exclude_kernel: bool,
}

/// Owner-CPU enable request. A sampling slot owns every IRQ-visible reference.
pub(super) struct SystemPmuEnable {
    pub(super) counter: Counter,
    pub(super) sampling: Option<(u32, SampleSlot)>,
}

/// State published only after the owner CPU has committed enable.
pub(super) struct SystemPmuEnableResult {
    pub(super) registration: Option<SampleRegistration>,
    pub(super) started_at: u64,
}

/// Value-only owner-CPU disable request.
pub(super) struct SystemPmuDisable {
    pub(super) counter: Counter,
    pub(super) registration: Option<SampleRegistration>,
}

/// Owner-consistent value and timestamp after a system event is quiescent.
pub(super) struct SystemPmuDisableResult {
    pub(super) value: u64,
    pub(super) stopped_at: u64,
}

/// Value-only owner-CPU read request.
pub(super) struct SystemPmuRead {
    pub(super) counter: Counter,
    pub(super) sampling: Option<alloc::sync::Arc<sampling::SamplingCount>>,
}

/// Owner-consistent raw count and timestamp.
pub(super) struct SystemPmuReadResult {
    pub(super) value: u64,
    pub(super) observed_at: u64,
}

/// Value-only owner-CPU reset request.
pub(super) struct SystemPmuReset {
    pub(super) counter: Counter,
    pub(super) sampling_period: Option<u32>,
}

/// Owner-CPU request to publish a newly attached output ring to one live slot.
pub(super) struct SystemPmuReplaceOutput {
    pub(super) registration: SampleRegistration,
    pub(super) output: SampleOutput,
}

/// Configures one reserved counter on the current owner CPU.
pub(super) fn configure_system_on_owner(request: SystemPmuConfigure) -> crate::StarryResult<()> {
    request
        .counter
        .configure(request.event, request.exclude_user, request.exclude_kernel)
}

/// Commits enable on the current owner CPU and returns its publication state.
pub(super) fn enable_system_on_owner(
    request: SystemPmuEnable,
) -> crate::StarryResult<SystemPmuEnableResult> {
    let registration = if let Some((period, slot)) = request.sampling {
        let Counter::Programmable(n) = request.counter else {
            return Err(crate::StarryError::BadState);
        };
        sampling::enable_local_pmu_irq().map_err(|_| crate::StarryError::NoSuchDevice)?;
        slot.count.preload(n, period);
        let registration =
            sampling::register(n, slot).map_err(|_| crate::StarryError::ResourceBusy)?;
        crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.enable_overflow_irq(id));
        crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.enable(id));
        Some(registration)
    } else {
        request.counter.enable();
        None
    };
    Ok(SystemPmuEnableResult {
        registration,
        started_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Quiesces one system-wide event on the current owner CPU.
pub(super) fn disable_system_on_owner(
    request: SystemPmuDisable,
) -> crate::StarryResult<SystemPmuDisableResult> {
    if let Some(registration) = request.registration {
        let Counter::Programmable(n) = request.counter else {
            return Err(crate::StarryError::BadState);
        };
        if registration.counter() != n {
            return Err(crate::StarryError::BadState);
        }
        crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.disable_overflow_irq(id));
        crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.disable(id));
        crate::perf::hw_owner::on_pmu(|pmu| pmu.clear_overflow(1u64 << n));
        sampling::unregister(registration).map_err(|_| crate::StarryError::BadState)?;
    } else {
        request.counter.disable();
    }
    Ok(SystemPmuDisableResult {
        value: request.counter.read(),
        stopped_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Reads one system-wide event on the current owner CPU.
pub(super) fn read_system_on_owner(
    request: SystemPmuRead,
) -> crate::StarryResult<SystemPmuReadResult> {
    Ok(SystemPmuReadResult {
        value: if let Some(sampling) = request.sampling {
            sampling.update(
                request
                    .counter
                    .programmable_index()
                    .ok_or(crate::StarryError::BadState)?,
            )
        } else {
            request.counter.read()
        },
        observed_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Resets one system-wide event on the current owner CPU.
pub(super) fn reset_system_on_owner(request: SystemPmuReset) -> crate::StarryResult<()> {
    match (request.counter, request.sampling_period) {
        (Counter::Programmable(n), Some(period)) => {
            crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.preload(id, u64::from(period)));
        }
        (counter, None) => counter.reset(),
        (Counter::Cycle, Some(_)) => return Err(crate::StarryError::BadState),
    }
    Ok(())
}

/// Replaces only the IRQ-visible output for one live sampling generation.
pub(super) fn replace_system_output_on_owner(
    request: SystemPmuReplaceOutput,
) -> crate::StarryResult<()> {
    sampling::replace_output(request.registration, request.output)
        .map_err(|_| crate::StarryError::BadState)
}

fn pmu_error(error: ax_cpu::pmu::PmuError) -> crate::StarryError {
    use ax_cpu::pmu::PmuError;
    match error {
        PmuError::Unavailable => crate::StarryError::NoSuchDevice,
        PmuError::UnsupportedEvent => crate::StarryError::Unsupported,
        PmuError::InvalidCounter | PmuError::InvalidConfiguration | PmuError::InvalidPeriod => {
            crate::StarryError::InvalidInput
        }
    }
}

/// Owner-local register transaction; no callback may wait or enable IRQs.
/// All callers below are bounded perf register operations on a reserved slot.
pub(in crate::perf) fn on_pmu<R>(operation: impl FnOnce(&mut ax_cpu::pmu::Pmu) -> R) -> R {
    let _guard = crate::sync::NoPreemptIrqSave::new();
    // SAFETY: the guard prevents migration, scheduling and IRQ reentry. Starry
    // is the PMU domain owner, and all event register access passes through
    // this function; the initializer has no remote or recursive access.
    unsafe { ax_hal::pmu::with_current(operation) }
        .expect("reserved PMU must remain available on its owner CPU")
}

pub(in crate::perf) fn on_counter<R>(
    index: usize,
    operation: impl FnOnce(
        &mut ax_cpu::pmu::Pmu,
        ax_cpu::pmu::CounterId,
    ) -> Result<R, ax_cpu::pmu::PmuError>,
) -> R {
    on_pmu(|pmu| {
        let counter = pmu
            .counter(index)
            .expect("owner reserved a valid PMU counter");
        operation(pmu, counter).expect("operation on reserved PMU counter")
    })
}

/// A whole-domain snapshot whose callbacks use separate bounded PMU sessions.
/// IRQ exclusion spans pause, reads and restoration; no session is borrowed
/// while a callback opens its own session through `on_counter`.
pub(in crate::perf) fn with_counters_paused<R>(operation: impl FnOnce() -> R) -> R {
    let _guard = crate::sync::NoPreemptIrqSave::new();
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            if self.0 {
                on_pmu(|pmu| pmu.start());
            }
        }
    }
    let running = on_pmu(|pmu| {
        let running = pmu.is_running();
        pmu.stop();
        running
    });
    let _restore = Restore(running);
    operation()
}
