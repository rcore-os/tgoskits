//! CPU-local ARM PMUv3 operations and value-only rendezvous requests.

use super::{
    sampling::{self, SampleSlot},
    sampling_lifecycle::SampleRegistration,
};

// Starry owns the complete PMU domain on every CPU. Initialize it once before
// the first reserved-slot operation; subsequent events must preserve live peers.
#[ax_percpu::def_percpu]
static INITIALIZED: bool = false;

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
        crate::perf::hw_owner::on_counter(n, |pmu, id| pmu.preload(id, u64::from(period)));
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
        value: request.counter.read(),
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
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            let mut pmu = ax_cpu::pmu::Pmu::current()
                .expect("reserved PMU must remain available");
            if !INITIALIZED.read_current(pin) {
                // No Starry event has touched this CPU's PMU yet. Retire
                // firmware enables/IRQs and EL0 access before publishing
                // initial state; never reset another live event.
                pmu.reset();
                INITIALIZED.write_current(pin, true);
            }
            operation(&mut pmu)
        })
    }
    .expect("perf PMU owner must have an installed CPU area")
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
