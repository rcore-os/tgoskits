//! Hardware-PMU `perf` facade.
//!
//! Architecture-independent dispatch keeps using this stable module path while
//! ARM PMUv3 ownership, allocation, event state, sampling, and open validation
//! live in focused implementation modules.

use ax_errno::AxResult;
use kbpf_basic::linux_bpf::perf_event_attr;

pub use super::hw_event::ARMV8_PMUV3_PERF_TYPE;
use super::target::PerfTarget;

/// Architecture-selected hardware perf event implementation.
pub type HwPerfEvent = super::hw_event::HwPerfEvent;

/// Opens one architecture-selected hardware perf event.
pub fn perf_event_open_hw(attr: &perf_event_attr, target: PerfTarget) -> AxResult<HwPerfEvent> {
    #[cfg(target_arch = "aarch64")]
    {
        super::hw_open::perf_event_open_hw(attr, target)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        super::hw_event::perf_event_open_hw(attr, target)
    }
}
#[cfg(target_arch = "aarch64")]
pub(crate) use super::hw_allocation::{alloc_programmable_counter, free_programmable_counter};
#[cfg(target_arch = "aarch64")]
pub(super) use super::hw_owner::{
    SystemPmuConfigure, SystemPmuDisable, SystemPmuEnable, SystemPmuEnableResult, SystemPmuRead,
    SystemPmuReadResult, SystemPmuReset, configure_system_on_owner, disable_system_on_owner,
    enable_system_on_owner, read_system_on_owner, reset_system_on_owner,
};
