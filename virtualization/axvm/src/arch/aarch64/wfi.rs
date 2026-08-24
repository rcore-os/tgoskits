//! WFI trapping policy for the AArch64 timer backend.

use crate::{Aarch64WfiPolicy, AxVmError, AxVmResult};

/// Hardware wake sources available while a guest remains inside WFI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TimerWakeCapabilities {
    pub(super) virtual_timer: bool,
    pub(super) physical_timer: bool,
}

/// The current world switch loads CNTV state directly, while guest CNTP state
/// remains software-emulated and needs a trapped WFI to arm the host timer.
pub(super) const TIMER_WAKE_CAPABILITIES: TimerWakeCapabilities = TimerWakeCapabilities {
    virtual_timer: true,
    physical_timer: false,
};

pub(super) fn resolve_trap_wfi(
    policy: Aarch64WfiPolicy,
    placed_on_dedicated_cpu: bool,
    capabilities: TimerWakeCapabilities,
    physical_timer_exposed: bool,
) -> AxVmResult<bool> {
    match policy {
        Aarch64WfiPolicy::Auto => Ok(trap_wfi(
            placed_on_dedicated_cpu,
            capabilities,
            physical_timer_exposed,
        )),
        Aarch64WfiPolicy::Trap => Ok(true),
        Aarch64WfiPolicy::Passthrough => {
            if !placed_on_dedicated_cpu {
                return Err(AxVmError::invalid_config(
                    "AArch64 WFI passthrough requires a dedicated host CPU",
                ));
            }
            if !capabilities.virtual_timer
                || (physical_timer_exposed && !capabilities.physical_timer)
            {
                return Err(AxVmError::invalid_config(
                    "AArch64 WFI passthrough requires hardware wake support for every exposed \
                     timer",
                ));
            }
            Ok(false)
        }
    }
}

pub(super) const fn trap_wfi(
    placed_on_dedicated_cpus: bool,
    capabilities: TimerWakeCapabilities,
    physical_timer_exposed: bool,
) -> bool {
    !placed_on_dedicated_cpus
        || !capabilities.virtual_timer
        || (physical_timer_exposed && !capabilities.physical_timer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_trap_policy_isolates_wfi_from_the_timer_contract() {
        assert!(matches!(
            resolve_trap_wfi(Aarch64WfiPolicy::Trap, true, TIMER_WAKE_CAPABILITIES, false,),
            Ok(true)
        ));
    }

    #[test]
    fn passthrough_policy_rejects_a_guest_without_hardware_timer_wake() {
        assert!(
            resolve_trap_wfi(
                Aarch64WfiPolicy::Passthrough,
                true,
                TIMER_WAKE_CAPABILITIES,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn passthrough_policy_rejects_a_shared_vcpu() {
        assert!(
            resolve_trap_wfi(
                Aarch64WfiPolicy::Passthrough,
                false,
                TimerWakeCapabilities {
                    virtual_timer: true,
                    physical_timer: true,
                },
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn dedicated_vcpu_still_traps_wfi_when_a_guest_timer_is_emulated() {
        assert!(trap_wfi(
            true,
            TimerWakeCapabilities {
                virtual_timer: true,
                physical_timer: false,
            },
            true,
        ));
    }

    #[test]
    fn shared_vcpu_always_traps_wfi() {
        assert!(trap_wfi(
            false,
            TimerWakeCapabilities {
                virtual_timer: true,
                physical_timer: true,
            },
            true,
        ));
    }

    #[test]
    fn dedicated_vcpu_may_wait_in_place_when_every_guest_timer_wakes_in_hardware() {
        assert!(!trap_wfi(
            true,
            TimerWakeCapabilities {
                virtual_timer: true,
                physical_timer: true,
            },
            true,
        ));
    }

    #[test]
    fn dedicated_virtual_only_guest_may_use_hardware_wfi() {
        assert!(!trap_wfi(
            true,
            TimerWakeCapabilities {
                virtual_timer: true,
                physical_timer: false,
            },
            false,
        ));
    }
}
