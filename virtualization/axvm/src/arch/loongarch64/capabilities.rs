//! LoongArch64 implementations of AxVM platform capability hooks.

use super::LoongArch64Arch;
use crate::architecture::{AddressSpacePlatform, DevicePlatform, HostTimePlatform};

impl DevicePlatform for LoongArch64Arch {}

impl AddressSpacePlatform for LoongArch64Arch {}

impl HostTimePlatform for LoongArch64Arch {
    fn set_oneshot_timer(_deadline_ns: u64) {}

    fn register_timer_callback() {
        ax_std::os::arceos::modules::ax_task::register_timer_callback(|_| {
            crate::check_timer_events();
        });
    }
}

pub fn host_fdt_bootarg() -> usize {
    ax_std::os::arceos::modules::ax_hal::dtb::get_bootarg()
}

pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt(paddr)
}
