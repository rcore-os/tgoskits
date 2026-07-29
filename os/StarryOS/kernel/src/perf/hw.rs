//! Hardware-PMU `perf` facade.
//!
//! Architecture-independent dispatch keeps using this stable module path while
//! ARM PMUv3 ownership, allocation, event state, sampling, and open validation
//! live in focused implementation modules.

use ax_errno::AxResult;
use kbpf_basic::linux_bpf::perf_event_attr;

pub use super::hw_event::ARMV8_PMUV3_PERF_TYPE;
use super::{access::AuthorizedPerfTarget, target::PerfTargetKind};

/// Counter resource selected by side-effect-free hardware validation.
#[cfg(target_arch = "aarch64")]
pub(super) enum ValidatedHwCounter {
    /// System-wide cycle event that prefers the dedicated counter.
    SystemPreferredCycle(u16),
    /// System-wide programmable counter with its ARM event number.
    SystemProgrammable(u16),
    /// Task-bound cycle event that prefers the dedicated counter.
    TaskPreferredCycle(u16),
    /// Task-bound programmable counter with its ARM event number.
    TaskProgrammable(u16),
}

/// Validated hardware-open inputs retained until authorization succeeds.
#[cfg(target_arch = "aarch64")]
pub(super) struct ValidatedHwOpen {
    pub(super) num_counters: usize,
    pub(super) counter: ValidatedHwCounter,
    pub(super) is_sampling: bool,
    pub(super) is_freq: bool,
    pub(super) sample_period: u32,
    pub(super) target_freq: u32,
}

/// Uninhabited-in-practice validation token on architectures without a PMU.
#[cfg(not(target_arch = "aarch64"))]
pub(super) struct ValidatedHwOpen;

/// Architecture-selected hardware perf event implementation.
pub type HwPerfEvent = super::hw_event::HwPerfEvent;

/// Validates hardware attributes before target authorization has side effects.
pub(super) fn validate_perf_event_open_hw(
    attr: &perf_event_attr,
    target_kind: PerfTargetKind,
) -> AxResult<ValidatedHwOpen> {
    #[cfg(target_arch = "aarch64")]
    {
        super::hw_open::validate_perf_event_open_hw(attr, target_kind)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (attr, target_kind);
        Err(ax_errno::AxError::Unsupported)
    }
}

/// Opens one architecture-selected hardware perf event.
pub(super) fn perf_event_open_hw(
    attr: &perf_event_attr,
    target: AuthorizedPerfTarget,
    validated: ValidatedHwOpen,
) -> AxResult<HwPerfEvent> {
    #[cfg(target_arch = "aarch64")]
    {
        super::hw_open::perf_event_open_hw(attr, target, validated)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        super::hw_event::perf_event_open_hw(attr, target, validated)
    }
}
#[cfg(target_arch = "aarch64")]
pub(crate) use super::hw_allocation::alloc_programmable_counter;
#[cfg(target_arch = "aarch64")]
pub(super) use super::hw_owner::{
    SystemPmuConfigure, SystemPmuDisable, SystemPmuDisableResult, SystemPmuEnable,
    SystemPmuEnableResult, SystemPmuRead, SystemPmuReadResult, SystemPmuReset,
    configure_system_on_owner, disable_system_on_owner, enable_system_on_owner,
    read_system_on_owner, reset_system_on_owner,
};
