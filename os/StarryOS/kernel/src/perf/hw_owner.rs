//! CPU-local ARM PMUv3 operations and value-only rendezvous requests.

use ax_errno::{AxError, AxResult};

use super::{
    sampling::{self, SampleSlot},
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
    ) -> AxResult<()> {
        match (self, event) {
            (Self::Cycle, None) => {
                ax_cpu::pmu::cycles::configure(exclude_user, exclude_kernel);
            }
            (Self::Programmable(n), Some(event)) => {
                ax_cpu::pmu::counter::configure(n, event, exclude_user, exclude_kernel);
            }
            _ => return Err(AxError::BadState),
        }
        Ok(())
    }

    pub(super) fn enable(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::enable(),
            Self::Programmable(n) => ax_cpu::pmu::counter::enable(n),
        }
    }

    pub(super) fn disable(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::disable(),
            Self::Programmable(n) => ax_cpu::pmu::counter::disable(n),
        }
    }

    pub(super) fn reset(self) {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::reset(),
            Self::Programmable(n) => ax_cpu::pmu::counter::reset(n),
        }
    }

    pub(super) fn read(self) -> u64 {
        match self {
            Self::Cycle => ax_cpu::pmu::cycles::read(),
            Self::Programmable(n) => ax_cpu::pmu::counter::read(n),
        }
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
pub(super) fn configure_system_on_owner(request: SystemPmuConfigure) -> AxResult<()> {
    ax_cpu::pmu::init_cpu();
    request
        .counter
        .configure(request.event, request.exclude_user, request.exclude_kernel)
}

/// Commits enable on the current owner CPU and returns its publication state.
pub(super) fn enable_system_on_owner(request: SystemPmuEnable) -> AxResult<SystemPmuEnableResult> {
    let registration = if let Some((period, slot)) = request.sampling {
        let Counter::Programmable(n) = request.counter else {
            return Err(AxError::BadState);
        };
        sampling::enable_local_pmu_irq().map_err(|_| AxError::NoSuchDevice)?;
        ax_cpu::pmu::counter::preload(n, period);
        let registration = sampling::register(n, slot).map_err(|_| AxError::ResourceBusy)?;
        ax_cpu::pmu::overflow::enable_irq(n);
        ax_cpu::pmu::counter::enable(n);
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
) -> AxResult<SystemPmuDisableResult> {
    if let Some(registration) = request.registration {
        let Counter::Programmable(n) = request.counter else {
            return Err(AxError::BadState);
        };
        if registration.counter() != n {
            return Err(AxError::BadState);
        }
        ax_cpu::pmu::overflow::disable_irq(n);
        ax_cpu::pmu::counter::disable(n);
        ax_cpu::pmu::overflow::clear(1 << n);
        sampling::unregister(registration).map_err(|_| AxError::BadState)?;
    } else {
        request.counter.disable();
    }
    Ok(SystemPmuDisableResult {
        value: request.counter.read(),
        stopped_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Reads one system-wide event on the current owner CPU.
pub(super) fn read_system_on_owner(request: SystemPmuRead) -> AxResult<SystemPmuReadResult> {
    Ok(SystemPmuReadResult {
        value: request.counter.read(),
        observed_at: ax_runtime::hal::time::monotonic_time_nanos(),
    })
}

/// Resets one system-wide event on the current owner CPU.
pub(super) fn reset_system_on_owner(request: SystemPmuReset) -> AxResult<()> {
    match (request.counter, request.sampling_period) {
        (Counter::Programmable(n), Some(period)) => {
            ax_cpu::pmu::counter::preload(n, period);
        }
        (counter, None) => counter.reset(),
        (Counter::Cycle, Some(_)) => return Err(AxError::BadState),
    }
    Ok(())
}
