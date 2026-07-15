//! x86_64 implementations of AxVM platform capability hooks.

use super::X86_64Arch;
use crate::architecture::{GuestBootPlatform, HostTimePlatform, VmTimerIntegration};

impl HostTimePlatform for X86_64Arch {
    // ArceOS also programs the LAPIC one-shot timer for scheduler deadlines. Sharing its timer
    // callback prevents either timer wheel from overwriting the other's next hardware deadline.
    const VM_TIMER_INTEGRATION: VmTimerIntegration = VmTimerIntegration::RuntimeCallback;
}

impl GuestBootPlatform for X86_64Arch {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_vm_timers_share_the_runtime_timer_source() {
        assert_eq!(
            <X86_64Arch as HostTimePlatform>::VM_TIMER_INTEGRATION,
            VmTimerIntegration::RuntimeCallback
        );
    }
}
